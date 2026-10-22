//! The command gate: an **allowlist** over shell tool calls.
//!
//! # This module fails closed. The rest of `block_bad_cmd` does not.
//!
//! `block_bad_cmd.rs` documents itself as "a friction-reducing nudge, not a
//! security sandbox", and that is accurate for what it does: `bad_commands`
//! and `bad_pipelines` enumerate *bad* shapes, so a gap in the enumeration
//! costs a nudge, and failing open at recursion depth or on a malformed
//! payload is the right call — a guard that cannot run must not wedge every
//! tool call.
//!
//! The gate inverts both properties, deliberately:
//!
//! - It is an **allowlist**. Exactly one command shape is permitted — every
//!   statement begins with the gate prefix (`tap` by default) — and anything
//!   else is denied. There is no enumeration to leave a gap in.
//! - It **fails closed**. A payload the gate cannot parse is a payload whose
//!   prefix it cannot verify, and allowing that would restore the silent
//!   permissive default the gate exists to remove.
//!
//! Do not "harmonize" a denial here into an allow for consistency with the
//! module next door. The asymmetry is the design.
//!
//! # Why an allowlist is worth the ergonomic cost
//!
//! The motivating failure is an agent emitting `rm -rf "$VAR"/` with `$VAR`
//! unset, which the shell expands to `rm -rf /`. `block_bad_cmd_rm_vars.rs`
//! attacks that by *proving*, from the command text, that a path variable
//! holds one nonempty literal path — 1100 lines of abstract interpretation,
//! where every parser bug is a bypass.
//!
//! The gate attacks it by *observation* instead. `tap` runs after the shell
//! has already expanded the command, so it receives the real argv: the
//! variable is already `/`, and there is nothing left to prove. The gate's
//! only job is to guarantee that `tap` is on the path of every command, which
//! is a far smaller question than the one the interpreter answers.
//!
//! # Uncertainty is cheap here
//!
//! When the interpreter cannot decide, denying blocks legitimate work, so it
//! is tuned to allow. When the gate cannot decide, denying costs the agent one
//! extra tool call — it splits a compound command into two. That asymmetry is
//! why this scanner refuses every construct it cannot decompose with
//! certainty (command substitution, subshells, process substitution, control
//! flow) rather than trying to reason about them.
//!
//! # Scope and residual risk
//!
//! - **Depth 1 only.** `tap make` confines nothing inside the Makefile; the
//!   gate sees `make` and stops there. This matches the threat model — the
//!   accident class is an agent slip *in the tool-call string itself*. Genuine
//!   containment of descendants is a sandbox's job (Landlock, container), not
//!   a parser's.
//! - **Redirections are the shell's, not `tap`'s.** `tap cmd > "$VAR/out"`
//!   truncates whatever the shell resolves. A redirect touches one file and
//!   cannot recurse, so it is a far smaller hazard than a recursive delete;
//!   `set -u` in the session shell is the proportionate mitigation.
//! - **Coverage is per-session.** The gate only enforces where clud set
//!   [`GATE_MODE_ENV`] in the session environment. A session clud did not
//!   launch is not gated, which is why `block_bad_cmd_rm_vars` stays in place.

use super::*;

/// Turns the gate on. clud sets this in the session environment at launch,
/// the same mechanism `CLUD_HOOK_DISPATCH` uses; a session clud did not launch
/// never sees it and keeps the pre-gate behavior.
pub(super) const GATE_MODE_ENV: &str = "CLUD_CMD_GATE";
/// Overrides the required prefix, for repos that route through their own
/// wrapper name.
pub(super) const GATE_PREFIX_ENV: &str = "CLUD_CMD_GATE_PREFIX";
/// The wrapper every gated statement must invoke.
pub(super) const DEFAULT_GATE_PREFIX: &str = "tap";

/// Shell builtins that change shell state rather than running a program.
///
/// These cannot be wrapped: `tap cd foo` execs `cd` in a child process, which
/// changes that child's directory and exits. Since they run no external code,
/// exempting them is sound — and `cd` specifically is already governed by
/// `block_bad_cmd_cd.rs`.
const EXEMPT_BUILTINS: &[&str] = &["cd", "export", "set", "unset", "true", "false", ":"];

/// Words that introduce compound commands. The flat scanner below splits on
/// operators, not syntax, so a control-flow body would arrive here as a
/// statement whose first word is `do` or `then`. Naming them produces an
/// actionable message instead of a confusing "must start with tap".
const CONTROL_KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "in", "function", "select", "time", "coproc",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GateMode {
    Off,
    Enforce,
}

pub(super) fn gate_mode() -> GateMode {
    gate_mode_from(std::env::var(GATE_MODE_ENV).ok().as_deref())
}

/// The env-var reading split out so the rollout default is testable without
/// mutating process environment, which is racy across parallel tests.
fn gate_mode_from(value: Option<&str>) -> GateMode {
    match value.map(str::trim) {
        Some("enforce") | Some("1") | Some("on") => GateMode::Enforce,
        _ => GateMode::Off,
    }
}

pub(super) fn gate_prefix() -> String {
    gate_prefix_from(std::env::var(GATE_PREFIX_ENV).ok().as_deref())
}

fn gate_prefix_from(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_GATE_PREFIX)
        .to_string()
}

/// Whether this tool's input is a shell command line the gate governs.
///
/// Non-shell tools (`Read`, `Edit`, `Write`, …) are the harness permission
/// layer's business, not the gate's.
pub(super) fn gates_tool(tool_name: &str) -> bool {
    matches!(
        tool_basename(tool_name).as_str(),
        "bash"
            | "powershell"
            | "pwsh"
            | "cmd"
            | "commandprompt"
            | "shell"
            | "shell_command"
            | "exec_command"
    )
}

/// Tool namespaces do not change the declared command payload contract.
pub(super) fn tool_basename(tool_name: &str) -> String {
    tool_name
        .rsplit('.')
        .next()
        .unwrap_or(tool_name)
        .to_ascii_lowercase()
}

/// Emit a gate denial in the harness's PreToolUse protocol and return its
/// exit code.
pub(super) fn gate_deny(reason: &str) -> i32 {
    let message = format!("[clud cmd-gate] {reason}");
    append_log(&format!("GATE-BLOCKED: {message}"));
    println!("{}", deny_json(reason));
    eprintln!("{message}");
    2
}

/// The reason a command is refused, or `None` when every statement is gated.
pub(super) fn gate_reason(command: &str, prefix: &str) -> Option<String> {
    if command.trim().is_empty() {
        return Some(refusal(
            "the tool call carried no command text, so the gate could not verify it",
            prefix,
        ));
    }

    let statements = match decompose(command) {
        Decomposition::Statements { top, .. } => top,
        Decomposition::ControlFlow(keyword) => {
            return Some(refusal(
                &format!(
                    "`{keyword}` introduces a compound command; the gate cannot prove every \
                     branch is wrapped"
                ),
                prefix,
            ));
        }
        Decomposition::Opaque(construct) => {
            return Some(refusal(
                &format!(
                    "the command uses {construct}, which runs a program the gate cannot see or \
                     wrap"
                ),
                prefix,
            ));
        }
    };

    if statements.is_empty() {
        return Some(refusal(
            "the command decomposed to no runnable statement",
            prefix,
        ));
    }

    for words in &statements {
        if let Some(reason) = statement_reason(words, prefix) {
            return Some(reason);
        }
    }
    None
}

/// How much work an agent's command needs before the gate admits it (#1067
/// step 2). The false-positive measurement replays real, ungated commands, so
/// almost none carry the prefix; what matters is how cheaply each could be
/// restated, not whether it passed as written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GateClass {
    /// Passes as written: already wrapped, or only exempt builtins and
    /// assignments.
    Allowed,
    /// One statement; prefixing it with the wrapper is the whole fix.
    PrefixSingle,
    /// Several statements joined by `;`, `&&`, `|` and the like; each needs
    /// the wrapper. A mechanical restatement, but a longer one.
    PrefixCompound,
    /// Refused even with every statement wrapped: the named construct
    /// (command substitution, a subshell, control flow, bad quoting, ...)
    /// has to be rewritten or split into separate tool calls.
    Restructure(&'static str),
}

/// Classify `command` for the replay measurement. Shares [`decompose`] and
/// the keyword and builtin tables with [`gate_reason`], so the two cannot
/// disagree about what the gate refuses.
pub fn classify(command: &str, prefix: &str) -> GateClass {
    if gate_reason(command, prefix).is_none() {
        return GateClass::Allowed;
    }
    if command.trim().is_empty() {
        return GateClass::Restructure("an empty command");
    }
    let programs = match decompose(command) {
        Decomposition::Statements { top, .. } => top,
        Decomposition::ControlFlow(_) => return GateClass::Restructure("control flow"),
        Decomposition::Opaque(construct) => return GateClass::Restructure(construct),
    };
    if programs.is_empty() {
        return GateClass::Restructure("no runnable statement");
    }
    for words in &programs {
        let bare = words[0].trim_matches(&['\'', '"'][..]);
        let bare = bare.rsplit(['/', '\\']).next().unwrap_or(bare);
        if CONTROL_KEYWORDS.contains(&bare.to_ascii_lowercase().as_str()) {
            return GateClass::Restructure("control flow");
        }
    }
    if programs.len() == 1 {
        GateClass::PrefixSingle
    } else {
        GateClass::PrefixCompound
    }
}

fn statement_reason(words: &[String], prefix: &str) -> Option<String> {
    // A pure assignment (`SP=/tmp`) runs no program, so it is no statement:
    // `tap` sees the expanded value when the variable is later used.
    let first = words.first()?;

    let leading = first.trim_matches(&['\'', '"'][..]);
    let bare = leading
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(leading)
        .to_string();

    if CONTROL_KEYWORDS.contains(&bare.to_ascii_lowercase().as_str()) {
        return Some(refusal(
            &format!(
                "`{bare}` introduces a compound command; the gate cannot prove every branch is \
                 wrapped"
            ),
            prefix,
        ));
    }
    if EXEMPT_BUILTINS.contains(&bare.as_str()) {
        return None;
    }
    if bare == prefix {
        return None;
    }

    Some(refusal(
        &format!("`{bare}` was not invoked through `{prefix}`"),
        prefix,
    ))
}

fn refusal(detail: &str, prefix: &str) -> String {
    format!(
        "Blocked by the clud command gate: {detail}. Every command in this session must run as \
         `{prefix} <command>`, one simple statement per tool call — no `&&`, `;`, `|`, command \
         substitution, or control flow. Split compound work into separate tool calls."
    )
}

/// The literal words of each statement in `command`, or the construct that
/// made it undecomposable. Shared with the `/grind` role caps, which need the
/// same refusal of anything that hides a program. Nested shells' statements
/// (`bash -c 'gh issue create'`) follow their launcher as statements of their
/// own, so a cap sees what the nested shell runs.
///
/// Control flow is not refused here. Each `for`/`while`/`if`/... appears as
/// a one-word statement naming its keyword, ahead of the statements it
/// runs, so an allowlist refuses it and a denylist can see a loop.
pub(super) fn statement_words(command: &str) -> Result<Vec<Vec<String>>, &'static str> {
    use crate::shell_ast::Construct;
    let script = parse_command(command, ShellDialect::Posix, None);
    if let Some(construct) = opaque_construct(&script) {
        return Err(construct);
    }
    let keywords = script
        .constructs
        .iter()
        .filter_map(|construct| match construct {
            Construct::ControlFlow(keyword) => Some(vec![(*keyword).to_string()]),
            _ => None,
        });
    // Nested shells' statements come before their launcher: `bash -c 'gh
    // issue close 12'` is judged as the `gh issue close` it runs, which gives
    // the most specific denial.
    let (top, nested) = statements(&script);
    Ok(keywords.chain(nested).chain(top).collect())
}

/// What the gate could make of a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Decomposition {
    /// The statements the shell runs: `top` in the command line itself,
    /// `nested` inside nested shells. Each is a program's literal words,
    /// leading `VAR=value` assignments excluded and nothing unwrapped: the
    /// gate needs the literal first word, or `env tap ...` would satisfy the
    /// prefix check while running something else.
    Statements {
        top: Vec<Vec<String>>,
        nested: Vec<Vec<String>>,
    },
    /// `if`, `for`, `while`, `case`, ...: a compound command.
    ControlFlow(&'static str),
    /// A construct the gate refuses to reason about, named for the message.
    Opaque(&'static str),
}

/// Split a command line into statements, or refuse.
///
/// Every [`Decomposition::Opaque`] is a construct that can run a program the
/// gate would never inspect — a command substitution's body, a subshell, a
/// process substitution — or text the shell itself could not parse.
/// Refusing costs one extra tool call; guessing costs the guarantee. The
/// command is parsed by `shell_ast` (#1962), so quoting, heredoc bodies,
/// redirections like `2>&1` and escaped operators are the grammar's
/// business, not a hand-written lexer's.
fn decompose(command: &str) -> Decomposition {
    use crate::shell_ast::Construct;
    let script = parse_command(command, ShellDialect::Posix, None);
    if let Some(construct) = opaque_construct(&script) {
        return Decomposition::Opaque(construct);
    }
    let control = script
        .constructs
        .iter()
        .find_map(|construct| match construct {
            Construct::ControlFlow(keyword) => Some(*keyword),
            _ => None,
        });
    if let Some(keyword) = control {
        return Decomposition::ControlFlow(keyword);
    }
    let (mut top, nested) = statements(&script);
    // A command made only of assignments (`SP=/tmp`) runs no program: one
    // empty statement, which needs no wrapper.
    if top.is_empty() && !script.assigned.is_empty() {
        top.push(Vec::new());
    }
    Decomposition::Statements { top, nested }
}

/// Text the shell cannot parse, or a construct that hides a program.
fn opaque_construct(script: &crate::shell_ast::Script) -> Option<&'static str> {
    use crate::shell_ast::Construct;
    if script.has_error {
        let error = script
            .syntax_error
            .unwrap_or(crate::shell_ast::SyntaxError::Other);
        return Some(error.describe());
    }
    script
        .constructs
        .iter()
        .find(|construct| !matches!(construct, Construct::ControlFlow(_)))
        .map(|construct| construct.describe())
}

/// The literal words of the top-level statements and of nested shells',
/// each followed by its redirections as written (`2>&1`, `>out.txt`,
/// `<<'EOF'`), which the `/grind` caps classify.
fn statements(script: &crate::shell_ast::Script) -> (Vec<Vec<String>>, Vec<Vec<String>>) {
    use crate::shell_ast::{Invocation, Origin};
    let words = |invocation: &Invocation| {
        let mut words = invocation.argv();
        words.extend(
            invocation
                .redirects
                .iter()
                .map(|redirect| format!("{}{}", redirect.operator, redirect.target.text())),
        );
        words
    };
    let top = script.top_level().map(&words).collect();
    let nested = script
        .invocations
        .iter()
        .filter(|inv| inv.origin == Origin::NestedShell)
        .map(words)
        .collect();
    (top, nested)
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    const P: &str = "tap";

    fn denied(command: &str) -> bool {
        gate_reason(command, P).is_some()
    }

    #[test]
    fn wrapped_simple_commands_pass() {
        for command in [
            "tap ls -la",
            "  tap ls -la  ",
            "tap echo 'a; b'",
            r#"tap echo "a && b""#,
            "tap grep -rn 'needle' .",
            "/usr/local/bin/tap ls",
            "./tap ls",
            "FOO=1 tap ls",
            "FOO=1 BAR=2 tap ls",
            "SP=/tmp/safe",
            "cd /home/niteris/dev/clud",
            "export FOO=bar",
            "tap echo '${VAR}'",
            "tap echo ${VAR}",
            "tap echo {a,b}",
            r#"tap sh -c 'echo hi'"#,
        ] {
            assert!(!denied(command), "expected allow for {command}");
        }
    }

    #[test]
    fn unwrapped_commands_are_denied() {
        for command in [
            "ls -la",
            "rm -rf /tmp/x",
            "make",
            "  git status  ",
            "/bin/ls",
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    #[test]
    fn every_statement_must_be_wrapped() {
        // The whole point of statement splitting: a wrapped head must not
        // launder an unwrapped tail.
        for command in [
            "tap ls && rm -rf /tmp/x",
            "tap ls; rm -rf /tmp/x",
            "tap ls || rm -rf /tmp/x",
            "tap ls | xargs rm",
            "tap ls & rm -rf /tmp/x",
            "tap ls\nrm -rf /tmp/x",
            "tap ls \r\n rm -rf /tmp/x",
            "cd /tmp && rm -rf x",
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    #[test]
    fn fully_wrapped_compounds_pass() {
        for command in [
            "tap ls && tap cat foo",
            "tap ls; tap cat foo",
            "cd /tmp && tap ls",
            "SP=/tmp; tap ls",
            "tap ls | tap grep needle",
        ] {
            assert!(!denied(command), "expected allow for {command}");
        }
    }

    #[test]
    fn constructs_that_hide_a_program_are_denied() {
        for command in [
            "tap echo $(rm -rf /tmp/x)",
            "tap echo `rm -rf /tmp/x`",
            r#"tap echo "$(rm -rf /tmp/x)""#,
            r#"tap echo "`rm -rf /tmp/x`""#,
            "tap diff <(ls) <(ls)",
            "tap tee >(cat)",
            "(rm -rf /tmp/x)",
            "tap ls; (rm -rf /tmp/x)",
            "{ rm -rf /tmp/x; }",
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    #[test]
    fn control_flow_is_denied() {
        for command in [
            "for f in *; do tap rm $f; done",
            "if true; then tap ls; fi",
            "while true; do tap ls; done",
            "case $x in a) tap ls;; esac",
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    #[test]
    fn transparent_wrappers_do_not_satisfy_the_prefix() {
        // Denylist rules unwrap these to find the real program; the gate
        // must not, or the prefix check becomes bypassable.
        for command in [
            "env tap ls",
            "exec tap ls",
            "command tap ls",
            "sudo tap ls",
            "env rm -rf /tmp/x",
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    #[test]
    fn malformed_quoting_is_denied() {
        for command in ["tap echo 'unterminated", r#"tap echo "unterminated"#] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    #[test]
    fn empty_command_is_denied() {
        for command in ["", "   ", "\n"] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    /// An `&` inside a redirection is not a statement separator. Splitting it
    /// made `2>&1` — which is everywhere — refuse with "`1` was not invoked
    /// through `tap`".
    #[test]
    fn redirection_ampersands_are_not_statement_separators() {
        for command in [
            "tap ls 2>&1",
            "tap ls >&2",
            "tap ls &> out",
            "tap ls >out 2>&1",
        ] {
            assert!(!denied(command), "expected allow for {command}");
        }
        // A real background/sequencing `&` must still split.
        for command in ["tap ls & rm -rf /tmp/x", "tap ls && rm -rf /tmp/x"] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    #[test]
    fn escaped_operators_are_not_structure() {
        // `\;` is an argument (find -exec), not a statement break, so the
        // statement stays one wrapped command.
        assert!(!denied(r"tap find . -name x -exec cat {} \;"));
    }

    #[test]
    fn heredoc_bodies_do_not_split_statements() {
        let command = "tap cat <<'EOF'\nrm -rf /\n&& ls\nEOF";
        assert!(
            !denied(command),
            "heredoc body must be masked, not parsed as statements"
        );
    }

    #[test]
    fn classify_separates_mechanical_restatements_from_restructures() {
        use GateClass::*;
        assert_eq!(classify("tap ls -la", P), Allowed);
        assert_eq!(classify("cd /tmp", P), Allowed);
        assert_eq!(classify("ls -la", P), PrefixSingle);
        assert_eq!(classify("cargo build 2>&1", P), PrefixSingle);
        assert_eq!(classify("git status && git diff", P), PrefixCompound);
        assert_eq!(classify("grep x f | head -5", P), PrefixCompound);
        assert_eq!(classify("tap git add . && git commit", P), PrefixCompound);
        assert_eq!(
            classify("echo $(date)", P),
            Restructure("command substitution")
        );
        assert_eq!(classify("(cd a && make)", P), Restructure("a subshell"));
        assert_eq!(
            classify("for f in *; do echo $f; done", P),
            Restructure("control flow")
        );
        assert_eq!(
            classify("echo 'open", P),
            Restructure("an unterminated single quote")
        );
        assert_eq!(classify("   ", P), Restructure("an empty command"));
        // Whatever classify calls restatable must really pass once wrapped.
        for (raw, wrapped) in [
            ("ls -la", "tap ls -la"),
            ("git status && git diff", "tap git status && tap git diff"),
            ("grep x f | head -5", "tap grep x f | tap head -5"),
        ] {
            assert!(classify(raw, P) != Allowed);
            assert_eq!(classify(wrapped, P), Allowed, "{wrapped}");
        }
    }

    #[test]
    fn prefix_is_configurable() {
        assert!(gate_reason("guard ls", "guard").is_none());
        assert!(gate_reason("tap ls", "guard").is_some());
    }

    #[test]
    fn denial_message_names_the_prefix_and_the_rule() {
        let reason = gate_reason("rm -rf /tmp/x", P).expect("denied");
        assert!(reason.contains("command gate"), "{reason}");
        assert!(reason.contains("`tap`"), "{reason}");
    }

    #[test]
    fn gated_tools_are_shell_tools_only() {
        for tool in ["Bash", "bash", "PowerShell", "cmd"] {
            assert!(gates_tool(tool), "{tool} should be gated");
        }
        for tool in ["Read", "Edit", "Write", "WebFetch"] {
            assert!(!gates_tool(tool), "{tool} should not be gated");
        }
    }

    /// Guards the rollout default. An absent variable — the state every
    /// existing session is in — must leave the gate off, or merging this
    /// module changes behavior everywhere.
    #[test]
    fn mode_is_off_unless_explicitly_enabled() {
        assert_eq!(gate_mode_from(None), GateMode::Off);
        for value in [
            "",
            "  ",
            "off",
            "0",
            "warn",
            "no",
            "enforce-later",
            "ENFORCE",
        ] {
            assert_eq!(
                gate_mode_from(Some(value)),
                GateMode::Off,
                "{value:?} must not enable the gate"
            );
        }
        for value in ["enforce", "1", "on", " enforce "] {
            assert_eq!(
                gate_mode_from(Some(value)),
                GateMode::Enforce,
                "{value:?} must enable the gate"
            );
        }
    }

    #[test]
    fn prefix_defaults_and_ignores_blank_overrides() {
        assert_eq!(gate_prefix_from(None), DEFAULT_GATE_PREFIX);
        assert_eq!(gate_prefix_from(Some("   ")), DEFAULT_GATE_PREFIX);
        assert_eq!(gate_prefix_from(Some(" guard ")), "guard");
    }
}
