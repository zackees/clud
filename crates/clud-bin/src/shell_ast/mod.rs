//! Shell command syntax trees plus a side-effect-free evaluator (#1962).
//!
//! The PreToolUse command scanner (`block_bad_cmd`) used to split command
//! text with a hand-written tokenizer and then match rules on word positions.
//! This module replaces that with real grammars: `tree-sitter-bash` for POSIX
//! shells, `tree-sitter-powershell` for PowerShell, and a small splitter for
//! cmd.exe, which has no usable grammar. Every dialect produces the same
//! [`Script`]: a flat list of [`Invocation`]s in source order, each with its
//! evaluated words, effective working directory and structural context
//! (pipeline, loop, subshell, substitution, nested shell).
//!
//! Evaluation resolves only what is knowable without running anything:
//! assignments and `export`, `$X` / `${X}` / `${X:-default}`, `~`, quoting,
//! `cd` through lists and subshells, `for` over literal items and the text of
//! `bash -c` / `pwsh -Command` / `cmd /c`. Anything else, such as `$(...)`,
//! `read` or a loop over command output, becomes [`Value::Unknown`] carrying
//! the source text, so a rule can tell "this is `rm`" from "this might be
//! anything". Nothing here spawns a process or touches the filesystem.
//!
//! Design notes live in `docs/architecture/shell-ast.md`.

mod bash;
mod cmd;
mod env;
mod paths;
mod powershell;
mod words;

pub use env::BaseEnv;

/// Substitutions and nested shells deeper than this are not walked.
pub const MAX_DEPTH: usize = words::MAX_DEPTH;

/// `rel` resolved against `base` as strings, `.`/`..` collapsed, in POSIX or
/// Windows spelling (whichever either side uses). Never touches the disk.
pub fn join_path(base: &str, rel: &str) -> String {
    paths::join(base, rel)
}

/// Which shell grammar to parse a command with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Posix,
    PowerShell,
    Cmd,
}

/// One evaluated shell word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// Fully known: quotes removed, variables substituted.
    Literal(String),
    /// Depends on something only known at run time.
    Unknown(Unknown),
}

/// Why a value could not be resolved, with the source text it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unknown {
    /// The word as written, with quotes removed and unresolved expansions
    /// left verbatim (`$R/x`, `$(git rev-parse)`). Rules that only need a
    /// best-effort string read this.
    pub raw: String,
    pub reason: UnknownReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownReason {
    /// `$(...)`, backticks, `<(...)` or PowerShell `$(...)`.
    CommandSubstitution,
    /// A variable that was never assigned and is not inherited.
    UnsetVariable,
    /// `$1`, `$@`, `$?`, `$RANDOM`, `$_` and friends.
    SpecialParameter,
    /// Assigned somewhere whose value depends on control flow or input
    /// (`read`, a branch, a loop over command output).
    RuntimeValue,
    /// Syntax this evaluator does not model (`${X/a/b}`, arithmetic, ...).
    Unsupported,
}

impl Value {
    pub fn literal(text: impl Into<String>) -> Self {
        Self::Literal(text.into())
    }

    pub fn unknown(raw: impl Into<String>, reason: UnknownReason) -> Self {
        Self::Unknown(Unknown {
            raw: raw.into(),
            reason,
        })
    }

    /// The literal text, or `None` when the value is unknown.
    pub fn as_literal(&self) -> Option<&str> {
        match self {
            Self::Literal(text) => Some(text),
            Self::Unknown(_) => None,
        }
    }

    /// The literal text, or the unknown value's source text.
    pub fn text(&self) -> &str {
        match self {
            Self::Literal(text) => text,
            Self::Unknown(unknown) => &unknown.raw,
        }
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown(_))
    }
}

/// The kind of loop an invocation runs inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopKind {
    /// `while ...; do ...; done`, PowerShell `while (...) {}`.
    While,
    /// `until ...; do ...; done`.
    Until,
    /// `for x in ...` / PowerShell `foreach (...)` / `ForEach-Object {}`.
    ForEach,
    /// `for ((...))` / PowerShell `for (...;...;...)`; `infinite` when the
    /// condition is empty (`for ((;;))`, `for (;;)`).
    CStyle { infinite: bool },
    /// PowerShell `do { } while/until (...)`.
    Do,
}

/// Where an invocation came from relative to the top-level script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Runs in the hook's own shell: top level, `{ ...; }`, `if`, loops.
    Script,
    /// `( ... )`: a child shell, so `cd` and assignments do not leak out.
    Subshell,
    /// `$(...)`, backticks, `<(...)`, `>(...)`, PowerShell `$(...)`.
    Substitution,
    /// The text of `bash -c`, `pwsh -Command`, `cmd /c`, `eval`, or a
    /// heredoc fed to a shell.
    NestedShell,
    /// A function body, which only runs if the function is called.
    FunctionBody,
}

/// This invocation's place in a pipeline of two or more stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipelineSlot {
    /// Shared by every stage of one pipeline; unique within the script.
    pub id: usize,
    pub stage: usize,
    pub stages: usize,
}

/// One redirection: its operator as written (descriptor included, such as
/// `2>`, `>>`, `&>`, `>&`) and its target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    pub operator: String,
    pub target: Value,
}

impl Redirect {
    /// Whether this redirection writes a file. Duplicating a descriptor
    /// (`2>&1`, `>&2`) and input redirections write nothing.
    pub fn writes_file(&self) -> bool {
        if !self.operator.contains('>') {
            return false;
        }
        let duplicates = match self.operator.split_once(">&") {
            // `>&2`, `2>&1`: the target is a descriptor (or `-`, closing it).
            Some((_, "")) => self.target.as_literal().is_some_and(|target| {
                target == "-" || target.chars().all(|ch| ch.is_ascii_digit())
            }),
            // PowerShell spells the merge as one operator: `2>&1`.
            Some((_, rest)) => rest.chars().all(|ch| ch.is_ascii_digit()),
            None => false,
        };
        !duplicates
    }
}

/// One simple command, evaluated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// `argv`, starting with the program. Leading `VAR=value` prefixes are
    /// in [`Invocation::env`], not here.
    pub words: Vec<Value>,
    /// Each word exactly as typed, quotes and expansions included, aligned
    /// with [`Invocation::words`]. Used to rebuild a command faithfully.
    pub raw: Vec<String>,
    /// `VAR=value` prefixes that apply to this command only.
    pub env: Vec<(String, Value)>,
    /// Redirections on this command (`> out`, `2>&1`, `< in`), in order.
    pub redirects: Vec<Redirect>,
    /// The body of a heredoc this command reads on stdin, exactly as
    /// written (`cat > w.sh <<'EOF'`, `python - <<EOF`). Data unless the
    /// command runs it; a body fed to a shell is also parsed as commands.
    pub heredoc: Option<String>,
    /// The working directory the command runs in.
    pub cwd: Value,
    pub pipeline: Option<PipelineSlot>,
    /// The innermost loop this command runs inside, if any.
    pub loop_kind: Option<LoopKind>,
    pub origin: Origin,
    /// Nesting depth of substitutions and nested shells (0 = top level).
    pub depth: usize,
    /// Run with a trailing `&`.
    pub background: bool,
    /// The grammar reported a syntax error in or around this command, so
    /// its words may be incomplete.
    pub in_error: bool,
    /// The command's own source text.
    pub source: String,
}

impl Invocation {
    /// Every word as text: literals verbatim, unknowns as their source.
    pub fn argv(&self) -> Vec<String> {
        self.words
            .iter()
            .map(|word| word.text().to_string())
            .collect()
    }

    /// The program word, lowercased, with any directory and `.exe` /
    /// `.cmd` / `.bat` / `.ps1` suffix removed. `None` when the program
    /// itself is unknown or there are no words.
    pub fn program(&self) -> Option<String> {
        self.words
            .first()
            .and_then(Value::as_literal)
            .map(paths::program_name)
    }

    /// `argv` with transparent launchers (`env`, `command`, `exec`, `sudo`,
    /// `nohup`, `time`, `timeout`, `nice`, `xargs`, `stdbuf`) and their
    /// options removed, so `env -i /usr/bin/git clone` reads as `git clone`.
    pub fn effective_argv(&self) -> &[Value] {
        words::strip_wrappers(&self.words)
    }

    /// [`Invocation::program`] of [`Invocation::effective_argv`].
    pub fn effective_program(&self) -> Option<String> {
        self.effective_argv()
            .first()
            .and_then(Value::as_literal)
            .map(paths::program_name)
    }

    /// Whether the program word is known.
    pub fn program_is_unknown(&self) -> bool {
        self.words.first().is_some_and(Value::is_unknown)
    }
}

/// A syntactic construct found at the top level of the script (not inside
/// a nested shell's quoted text), in source order. Rules that must refuse
/// what they cannot see through, like the command gate, read these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Construct {
    CommandSubstitution,
    BacktickSubstitution,
    ProcessSubstitution,
    Subshell,
    BraceGroup,
    /// `if`, `case`, `while`, `until`, `for`, `select` and PowerShell's
    /// statements of the same names; the keyword is lowercase.
    ControlFlow(&'static str),
    FunctionDefinition,
}

impl Construct {
    /// How a denial names the construct.
    pub fn describe(self) -> &'static str {
        match self {
            Self::CommandSubstitution => "command substitution",
            Self::BacktickSubstitution => "backtick command substitution",
            Self::ProcessSubstitution => "process substitution",
            Self::Subshell => "a subshell",
            Self::BraceGroup => "a brace group",
            Self::ControlFlow(_) => "control flow",
            Self::FunctionDefinition => "a function definition",
        }
    }
}

/// The first syntax error the grammar reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxError {
    UnterminatedSingleQuote,
    UnterminatedDoubleQuote,
    Other,
}

impl SyntaxError {
    /// How a denial names the error.
    pub fn describe(self) -> &'static str {
        match self {
            Self::UnterminatedSingleQuote => "an unterminated single quote",
            Self::UnterminatedDoubleQuote => "an unterminated double quote",
            Self::Other => "text the shell cannot parse",
        }
    }
}

/// A parsed and evaluated command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    pub dialect: Dialect,
    pub invocations: Vec<Invocation>,
    pub constructs: Vec<Construct>,
    /// Every loop in the script, at any depth.
    pub loops: Vec<LoopKind>,
    /// Every variable the script assigns, exports or unsets as a statement
    /// (not `VAR=x cmd` prefixes, which are in [`Invocation::env`]), at any
    /// depth, in source order.
    pub assigned: Vec<String>,
    /// The grammar reported at least one syntax error.
    pub has_error: bool,
    /// The first of those errors, classified.
    pub syntax_error: Option<SyntaxError>,
    /// Nesting went past [`MAX_DEPTH`], so the innermost commands were not
    /// walked. Callers fail open on them and should say so.
    pub depth_capped: bool,
}

impl Script {
    /// Commands that run in the script's own shell at the top level: not in
    /// a substitution, nested shell, subshell or function body.
    pub fn top_level(&self) -> impl Iterator<Item = &Invocation> {
        self.invocations
            .iter()
            .filter(|inv| inv.depth == 0 && inv.origin == Origin::Script)
    }

    /// Every loop kind the script contains, including loops whose bodies
    /// run no command.
    pub fn loop_kinds(&self) -> impl Iterator<Item = LoopKind> + '_ {
        self.loops.iter().copied()
    }
}

/// Parse and evaluate `text`, starting from `base` (inherited environment
/// variables and working directory).
pub fn parse(text: &str, dialect: Dialect, base: &BaseEnv) -> Script {
    let mut env = env::Env::from_base(base, dialect);
    let mut out = words::Collector::default();
    words::walk_text(text, dialect, &mut env, words::Ctx::top(), &mut out);
    out.into_script(dialect)
}

#[cfg(test)]
#[path = "fuzz_tests.rs"]
mod fuzz_tests;
#[cfg(test)]
#[path = "oracle_tests.rs"]
mod oracle_tests;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
