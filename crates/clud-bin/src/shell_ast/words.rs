//! State shared by every dialect's walker: the walk context, the output
//! collector, launcher unwrapping and nested-shell dispatch.

use super::env::Env;
use super::{
    bash, cmd, paths, powershell, Construct, Dialect, Invocation, LoopKind, Origin, PipelineSlot,
    Redirect, Script, SyntaxError, UnknownReason, Value,
};

/// Nested substitutions and shells deeper than this are not walked. The
/// scanner is a friction-reducing hook, not a sandbox, and pathological
/// input must not recurse without bound.
pub(super) const MAX_DEPTH: usize = 8;

/// Syntax nested deeper than this (`( ( ( ... ) ) )`, nested arrays) is not
/// walked, so pathological input cannot exhaust the stack.
pub(super) const MAX_NESTING: usize = 200;

/// Where the walker is: inherited by every invocation it emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Ctx {
    pub(super) origin: Origin,
    pub(super) depth: usize,
    pub(super) loop_kind: Option<LoopKind>,
    pub(super) pipeline: Option<PipelineSlot>,
    pub(super) background: bool,
    pub(super) in_error: bool,
}

impl Ctx {
    pub(super) fn top() -> Self {
        Self {
            origin: Origin::Script,
            depth: 0,
            loop_kind: None,
            pipeline: None,
            background: false,
            in_error: false,
        }
    }

    /// One level down into a substitution or nested shell.
    pub(super) fn nested(self, origin: Origin) -> Self {
        Self {
            origin,
            depth: self.depth + 1,
            pipeline: None,
            background: false,
            ..self
        }
    }

    pub(super) fn with_origin(self, origin: Origin) -> Self {
        Self { origin, ..self }
    }

    pub(super) fn in_loop(self, kind: LoopKind) -> Self {
        Self {
            loop_kind: Some(kind),
            ..self
        }
    }

    pub(super) fn in_pipeline(self, slot: PipelineSlot) -> Self {
        Self {
            pipeline: Some(slot),
            ..self
        }
    }

    pub(super) fn backgrounded(self) -> Self {
        Self {
            background: true,
            ..self
        }
    }

    pub(super) fn errored(self) -> Self {
        Self {
            in_error: true,
            ..self
        }
    }
}

/// Everything a walk produces.
#[derive(Debug, Default)]
pub(super) struct Collector {
    invocations: Vec<Invocation>,
    constructs: Vec<Construct>,
    loops: Vec<LoopKind>,
    assigned: Vec<String>,
    pub(super) has_error: bool,
    pub(super) depth_capped: bool,
    syntax_error: Option<SyntaxError>,
    next_pipeline: usize,
}

/// One command as the walker found it, before it is recorded.
pub(super) struct Found<'a> {
    pub(super) words: Vec<Value>,
    pub(super) raw: Vec<String>,
    pub(super) assignments: Vec<(String, Value)>,
    pub(super) redirects: Vec<Redirect>,
    pub(super) source: &'a str,
}

impl Collector {
    pub(super) fn pipeline_id(&mut self) -> usize {
        self.next_pipeline += 1;
        self.next_pipeline
    }

    /// Note a construct written at the top level of the script.
    pub(super) fn construct(&mut self, ctx: Ctx, construct: Construct) {
        if ctx.depth == 0 {
            self.constructs.push(construct);
        }
    }

    /// Note a statement-level assignment, export or unset of `name`.
    pub(super) fn assigned(&mut self, name: &str) {
        self.assigned.push(name.to_string());
    }

    /// Record a syntax error; only the first one is kept.
    pub(super) fn syntax_error(&mut self, error: SyntaxError) {
        self.has_error = true;
        self.syntax_error.get_or_insert(error);
    }

    pub(super) fn loop_seen(&mut self, kind: LoopKind) {
        self.loops.push(kind);
    }

    pub(super) fn last(&self) -> Option<&Invocation> {
        self.invocations.last()
    }

    pub(super) fn len(&self) -> usize {
        self.invocations.len()
    }

    /// Attach `redirects` to the commands recorded since `start` at `depth`:
    /// `{ a; b; } > out` redirects both.
    /// Give the first command recorded since `start` at `depth` the heredoc
    /// `body` it reads on stdin.
    pub(super) fn heredoc_since(&mut self, start: usize, depth: usize, body: &str) {
        if let Some(invocation) = self
            .invocations
            .iter_mut()
            .skip(start)
            .find(|invocation| invocation.depth == depth)
        {
            invocation.heredoc.get_or_insert_with(|| body.to_string());
        }
    }

    pub(super) fn redirect_since(&mut self, start: usize, depth: usize, redirects: &[Redirect]) {
        for invocation in self.invocations.iter_mut().skip(start) {
            if invocation.depth == depth {
                invocation.redirects.extend(redirects.iter().cloned());
            }
        }
    }

    /// Record one command; `false` (and nothing recorded) when it has no
    /// words.
    pub(super) fn emit(&mut self, found: Found<'_>, env: &Env, ctx: Ctx) -> bool {
        if found.words.is_empty() {
            return false;
        }
        self.invocations.push(Invocation {
            words: found.words,
            raw: found.raw,
            env: found.assignments,
            redirects: found.redirects,
            heredoc: None,
            cwd: env.cwd.clone(),
            pipeline: ctx.pipeline,
            loop_kind: ctx.loop_kind,
            origin: ctx.origin,
            depth: ctx.depth,
            background: ctx.background,
            in_error: ctx.in_error,
            source: found.source.trim().to_string(),
        });
        true
    }

    pub(super) fn into_script(self, dialect: Dialect) -> Script {
        Script {
            dialect,
            invocations: self.invocations,
            constructs: self.constructs,
            loops: self.loops,
            assigned: self.assigned,
            has_error: self.has_error,
            syntax_error: self.syntax_error,
            depth_capped: self.depth_capped,
        }
    }
}

/// Classify the first `ERROR` or `MISSING` node under `root`, in source
/// order. Shared by the tree-sitter dialects.
pub(super) fn first_syntax_error(root: tree_sitter::Node<'_>, src: &str) -> SyntaxError {
    let mut cursor = root.walk();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_missing() {
            return match node.kind() {
                "\"" => SyntaxError::UnterminatedDoubleQuote,
                "'" => SyntaxError::UnterminatedSingleQuote,
                _ => SyntaxError::Other,
            };
        }
        if node.is_error() {
            let text = node
                .utf8_text(src.as_bytes())
                .unwrap_or_default()
                .trim_start();
            return match text.chars().next() {
                Some('\'') => SyntaxError::UnterminatedSingleQuote,
                Some('"') => SyntaxError::UnterminatedDoubleQuote,
                _ => SyntaxError::Other,
            };
        }
        let children: Vec<_> = node.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    SyntaxError::Other
}

/// Accumulates the pieces of one word. Unknown pieces keep their source
/// text, so an unknown word still reads as what was written.
#[derive(Debug, Default)]
pub(super) struct Parts {
    text: String,
    unknown: Option<UnknownReason>,
}

impl Parts {
    pub(super) fn push_text(&mut self, text: &str) {
        self.text.push_str(text);
    }

    pub(super) fn push(&mut self, value: &Value) {
        match value {
            Value::Literal(text) => self.text.push_str(text),
            Value::Unknown(unknown) => {
                self.text.push_str(&unknown.raw);
                self.unknown.get_or_insert(unknown.reason);
            }
        }
    }

    pub(super) fn finish(self) -> Value {
        match self.unknown {
            None => Value::Literal(self.text),
            Some(reason) => Value::unknown(self.text, reason),
        }
    }
}

fn literal_program(word: Option<&Value>) -> Option<String> {
    word.and_then(Value::as_literal).map(paths::program_name)
}

fn is_option(word: &Value) -> bool {
    word.as_literal()
        .is_some_and(|text| text.starts_with('-') && text.len() > 1)
}

/// Skip leading options; options in `with_value` consume the next word
/// unless written `--opt=value` or with the value attached (`-n1`).
fn skip_options(words: &[Value], with_value: &[&str]) -> usize {
    let mut index = 0;
    while let Some(word) = words.get(index) {
        let Some(text) = word.as_literal() else { break };
        if text == "--" {
            return index + 1;
        }
        if !is_option(word) {
            break;
        }
        let takes_value = with_value.contains(&text);
        index += if takes_value { 2 } else { 1 };
    }
    index
}

/// The command a launcher runs, as an index into `words`; `None` when
/// `words[0]` is not a launcher or wraps nothing.
fn launcher_payload(words: &[Value]) -> Option<usize> {
    let program = literal_program(words.first())?;
    let rest = &words[1..];
    let skipped = match program.as_str() {
        "env" => skip_env_prefix(rest),
        "command" => {
            if rest
                .iter()
                .take_while(|w| is_option(w))
                .any(|w| matches!(w.as_literal(), Some("-v" | "-V")))
            {
                return None;
            }
            skip_options(rest, &[])
        }
        "exec" => skip_options(rest, &["-a"]),
        "sudo" | "doas" => skip_options(
            rest,
            &[
                "-u", "-g", "-h", "-p", "-U", "-C", "-D", "-r", "-t", "-T", "--user", "--group",
            ],
        ),
        "nohup" | "setsid" | "time" => skip_options(rest, &[]),
        "nice" => skip_options(rest, &["-n", "--adjustment"]),
        "ionice" => skip_options(rest, &["-c", "-n", "-p", "-P", "-u"]),
        "stdbuf" => skip_options(rest, &["-i", "-o", "-e"]),
        "timeout" => {
            let options = skip_options(rest, &["-s", "-k", "--signal", "--kill-after"]);
            options + 1
        }
        "xargs" => skip_options(
            rest,
            &[
                "-n",
                "-L",
                "-P",
                "-I",
                "-i",
                "-d",
                "-E",
                "-e",
                "-s",
                "-a",
                "--max-args",
                "--max-lines",
                "--max-procs",
                "--delimiter",
                "--arg-file",
            ],
        ),
        _ => return None,
    };
    let start = 1 + skipped;
    (start < words.len()).then_some(start)
}

/// `env` options and `NAME=value` pairs before the command.
fn skip_env_prefix(words: &[Value]) -> usize {
    let mut index = 0;
    while let Some(word) = words.get(index) {
        let Some(text) = word.as_literal() else { break };
        if text == "--" {
            return index + 1;
        }
        if matches!(
            text,
            "-u" | "-C" | "-S" | "--unset" | "--chdir" | "--split-string"
        ) {
            index += 2;
        } else if is_option(word) || is_assignment(text) {
            index += 1;
        } else {
            break;
        }
    }
    index
}

fn is_assignment(text: &str) -> bool {
    let Some((name, _)) = text.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

/// `words` with transparent launchers peeled off.
pub(super) fn strip_wrappers(words: &[Value]) -> &[Value] {
    let mut current = words;
    for _ in 0..8 {
        match launcher_payload(current) {
            Some(start) => current = &current[start..],
            None => break,
        }
    }
    current
}

/// Additional launchers that run a shell: `busybox sh`, `uv run bash`,
/// `find ... -exec sh -c ...`. Only used to find nested shells.
fn strip_shell_launchers(words: &[Value]) -> &[Value] {
    let mut current = strip_wrappers(words);
    for _ in 0..8 {
        let next = match literal_program(current.first()).as_deref() {
            Some("busybox") if current.len() > 1 => &current[1..],
            Some("uv") if current.get(1).and_then(Value::as_literal) == Some("run") => {
                uv_run_payload(&current[2..])
            }
            Some("find") => find_exec(current).unwrap_or(&[]),
            _ => break,
        };
        if next.is_empty() {
            break;
        }
        current = strip_wrappers(next);
    }
    current
}

/// What `uv run [options] [--] <command...>` runs. `--script` runs a file,
/// not a command, so it yields nothing.
fn uv_run_payload(words: &[Value]) -> &[Value] {
    use crate::block_bad_cmd::{UV_RUN_OPTIONS_WITH_VALUE, UV_RUN_SHORT_OPTIONS_WITH_VALUE};
    let mut index = 0;
    while let Some(text) = words.get(index).and_then(Value::as_literal) {
        if text == "--" {
            index += 1;
            break;
        }
        if text == "--script" || text.starts_with("--script=") {
            return &[];
        }
        if !text.starts_with('-') {
            break;
        }
        let takes_value = (!text.contains('=') && UV_RUN_OPTIONS_WITH_VALUE.contains(&text))
            || UV_RUN_SHORT_OPTIONS_WITH_VALUE.contains(&text);
        index += if takes_value { 2 } else { 1 };
    }
    words.get(index..).unwrap_or_default()
}

fn find_exec(words: &[Value]) -> Option<&[Value]> {
    let start = words.iter().position(|w| {
        matches!(
            w.as_literal(),
            Some("-exec" | "-execdir" | "-ok" | "-okdir")
        )
    })? + 1;
    let end = words[start..]
        .iter()
        .position(|w| matches!(w.as_literal(), Some(";" | "\\;" | "+")))
        .map_or(words.len(), |offset| start + offset);
    (end > start).then(|| &words[start..end])
}

const POSIX_SHELLS: &[&str] = &["bash", "sh", "zsh", "dash", "ksh", "ash", "mksh"];

pub(super) fn is_posix_shell(program: &str) -> bool {
    POSIX_SHELLS.contains(&program)
}

/// Join literal words with spaces; `None` if any is unknown.
fn joined(words: &[Value]) -> Option<String> {
    let mut parts = Vec::with_capacity(words.len());
    for word in words {
        parts.push(word.as_literal()?);
    }
    Some(parts.join(" "))
}

/// The script a shell invocation runs as code, with its dialect and whether
/// it runs in the current shell (`eval`) rather than a child.
fn nested_script(words: &[Value]) -> Option<(String, Dialect, bool)> {
    let words = strip_shell_launchers(words);
    let program = literal_program(words.first())?;
    let rest = &words[1..];
    if program == "eval" {
        return joined(rest).map(|text| (text, Dialect::Posix, true));
    }
    if is_posix_shell(&program) {
        let index = rest.iter().position(|word| {
            word.as_literal()
                .is_some_and(|t| t.starts_with('-') && !t.starts_with("--") && t.contains('c'))
        })?;
        let script = rest.get(index + 1)?.as_literal()?;
        return Some((script.to_string(), Dialect::Posix, false));
    }
    if program == "pwsh" || program == "powershell" {
        let index = rest.iter().position(|word| {
            word.as_literal().is_some_and(|t| {
                matches!(
                    t.to_ascii_lowercase().as_str(),
                    "-command" | "-c" | "/c" | "-com"
                )
            })
        })?;
        return joined(&rest[index + 1..]).map(|text| (text, Dialect::PowerShell, false));
    }
    if program == "cmd" {
        let index = rest.iter().position(|word| {
            word.as_literal()
                .is_some_and(|t| matches!(t.to_ascii_lowercase().as_str(), "/c" | "/k" | "/r"))
        })?;
        return joined(&rest[index + 1..]).map(|text| (text, Dialect::Cmd, false));
    }
    None
}

/// If `words` hands code to another shell, walk that code. `env` is the
/// calling shell's environment: `eval` runs in it, a child shell starts
/// from its exported variables.
pub(super) fn walk_nested(words: &[Value], env: &mut Env, ctx: Ctx, out: &mut Collector) {
    let Some((script, dialect, same_shell)) = nested_script(words) else {
        return;
    };
    if ctx.depth >= MAX_DEPTH {
        out.depth_capped = true;
        return;
    }
    let inner = ctx.nested(Origin::NestedShell);
    if same_shell {
        walk_text(&script, dialect, env, inner, out);
    } else {
        let mut child = env.child_shell(dialect);
        walk_text(&script, dialect, &mut child, inner, out);
    }
}

/// Walk `text` in `dialect`, appending to `out`.
pub(super) fn walk_text(
    text: &str,
    dialect: Dialect,
    env: &mut Env,
    ctx: Ctx,
    out: &mut Collector,
) {
    match dialect {
        Dialect::Posix => bash::walk_text(text, env, ctx, out),
        Dialect::PowerShell => powershell::walk_text(text, env, ctx, out),
        Dialect::Cmd => cmd::walk_text(text, env, ctx, out),
    }
}

/// Apply the shell-state effects of a builtin (`cd`, `pushd`, `popd`).
/// Shared because PowerShell and cmd spell the same operations; a name only
/// counts in the dialect that has it (`sl` is just a program to bash).
pub(super) fn apply_directory_builtin(
    dialect: Dialect,
    program: &str,
    args: &[Value],
    env: &mut Env,
) -> bool {
    let program = match (dialect, program) {
        (_, "cd" | "pushd" | "popd") => program,
        (Dialect::PowerShell, "chdir" | "set-location" | "sl") => "cd",
        (Dialect::PowerShell, "push-location") => "pushd",
        (Dialect::PowerShell, "pop-location") => "popd",
        (Dialect::Cmd, "chdir") => "cd",
        _ => return false,
    };
    let positional = args.iter().find(|word| !is_option(word));
    match program {
        "cd" => {
            match positional {
                Some(target) if target.as_literal() == Some("-") => {
                    env.cwd = Value::unknown("$OLDPWD", UnknownReason::RuntimeValue);
                }
                Some(target) => env.change_dir(target),
                None => {
                    if let Some(home) = env.home() {
                        env.change_dir(&Value::literal(home));
                    }
                }
            }
            true
        }
        "pushd" => {
            match positional {
                Some(target) => env.push_dir(target),
                None => env.push_dir(&env.cwd.clone()),
            }
            true
        }
        "popd" => {
            env.pop_dir();
            true
        }
        _ => false,
    }
}
