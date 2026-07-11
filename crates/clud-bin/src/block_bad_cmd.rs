//! Native `block-bad-cmd` PreToolUse hook.
//!
//! The hot path is a dedicated Rust binary (`clud-block-bad-cmd`) so hook
//! fires do not launch Python or uv.

use crate::repo_clud_config::{compile_match_pattern, BadCommandRule, MatchMode};
use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Cap on `$(...)`/backtick/subshell recursion depth (zackees/clud#519).
/// Past this depth the hook fails open (allows, logs a warning) rather
/// than denying or risking a stack overflow on pathological input —
/// this hook is a friction-reducing nudge, not a security sandbox.
const MAX_SUBSTITUTION_RECURSION_DEPTH: usize = 8;
/// Env var read for the `bad_commands` override escape hatch. Read
/// only from the real process environment, never parsed out of the
/// command text — see zackees/clud#519 comment thread for why
/// text-parsing this would race `command_words()`'s own env-assignment
/// stripping.
const BAD_CMD_OVERRIDE_ENV: &str = "CLUD_BAD_CMD_OVERRIDE";

pub const STDIN_READ_CHUNK_BYTES: usize = 64 * 1024;
pub const STDIN_READ_MAX_BYTES: usize = 1024 * 1024;
const DEFAULT_STDIN_READ_IDLE_TIMEOUT_SEC: f64 = 0.25;
const DEFAULT_STDIN_READ_DEADLINE_SEC: f64 = 2.0;
const LOG_REL_PATH: &str = ".clud/tools/hooks/block-bad-cmd.log";
const SENTINEL_PHRASE: &str = concat!("bad", " cmd");

const TOOL_RS_BUILD: &str = concat!("car", "go");
const TOOL_RS_COMPILER: &str = concat!("rust", "c");
const TOOL_RS_FORMAT: &str = concat!("rust", "fmt");
const TOOL_RS_RUNNER: &str = concat!("rust", "up");

const RUST_TOOLS: &[&str] = &[
    TOOL_RS_BUILD,
    TOOL_RS_COMPILER,
    TOOL_RS_FORMAT,
    concat!("clippy", "-driver"),
    concat!("car", "go", "-clippy"),
    concat!("car", "go", "-fmt"),
    TOOL_RS_RUNNER,
    concat!("rust", "doc"),
    concat!("rust", "-gdb"),
    concat!("rust", "-lldb"),
    concat!("rust", "-analyzer"),
];

const LEGACY_RUST_TRAMPOLINES: &[&str] = &[
    concat!("_car", "go"),
    concat!("_rust", "c"),
    concat!("_rust", "fmt"),
];
const SHELL_WRAPPERS: &[&str] = &["cmd", "powershell", "pwsh", "bash", "sh", "zsh", "eval"];

const UV_RUN_OPTIONS_WITH_VALUE: &[&str] = &[
    "--allow-insecure-host",
    "--cache-dir",
    "--color",
    "--config-setting",
    "--config-settings-package",
    "--config-file",
    "--default-index",
    "--directory",
    "--env-file",
    "--exclude-newer-package",
    "--exclude-newer",
    "--extra",
    "--extra-index-url",
    "--find-links",
    "--fork-strategy",
    "--group",
    "--gui-script",
    "--index",
    "--index-url",
    "--index-strategy",
    "--keyring-provider",
    "--link-mode",
    "--module",
    "--no-binary-package",
    "--no-build-isolation-package",
    "--no-build-package",
    "--no-editable-package",
    "--no-extra",
    "--no-group",
    "--no-sources-package",
    "--only-group",
    "--package",
    "--prerelease",
    "--project",
    "--python",
    "--python-platform",
    "--refresh-package",
    "--reinstall-package",
    "--resolution",
    "--script",
    "--upgrade-group",
    "--upgrade-package",
    "--with",
    "--with-editable",
    "--with-requirements",
];
const UV_RUN_SHORT_OPTIONS_WITH_VALUE: &[&str] = &["-C", "-P", "-f", "-i", "-m", "-p", "-s", "-w"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookPayloadView {
    pub tool_name: String,
    pub command: String,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny { reason: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandEvaluation {
    pub reason: Option<String>,
    pub warnings: Vec<String>,
    pub log_messages: Vec<String>,
}

#[derive(Debug, Clone)]
struct StdinRead {
    text: String,
    log_messages: Vec<String>,
}

pub fn run() -> i32 {
    let stdin = read_stdin_bounded();
    for message in &stdin.log_messages {
        append_log(message);
    }
    append_log(&format!("raw_stdin_bytes={}", stdin.text.len()));

    let payload: Value = match serde_json::from_str(if stdin.text.trim().is_empty() {
        "{}"
    } else {
        &stdin.text
    }) {
        Ok(value) => value,
        Err(error) => {
            append_log(&format!("json_decode_error: {error}"));
            return 0;
        }
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(payload) = parse_payload_value(&payload, &cwd) else {
        append_log("unsupported_payload_shape");
        return 0;
    };
    append_log(&format!(
        "tool_name={:?} cwd={:?} command={:?}",
        payload.tool_name,
        payload.cwd.to_string_lossy(),
        payload.command
    ));

    let bad_commands = crate::repo_clud_config::discover_effective_clud_config(&payload.cwd)
        .map(|cfg| cfg.bad_commands)
        .unwrap_or_default();

    let allow_hybrid_uv_run = std::env::var("CLUD_UV_RUST_ALLOW_ALL").ok().as_deref() == Some("1");
    let evaluation = evaluate_command(
        &payload.command,
        Some(&payload.cwd),
        allow_hybrid_uv_run,
        &bad_commands,
    );
    for message in &evaluation.log_messages {
        append_log(message);
    }
    for warning in &evaluation.warnings {
        eprintln!("{warning}");
    }

    if let Some(reason) = evaluation.reason {
        let msg = format!(
            "[block-bad-cmd hook] refusing to run {:?}: {reason}",
            payload.tool_name
        );
        append_log(&format!("BLOCKED: {msg}"));
        println!("{}", deny_json(&reason));
        eprintln!("{msg}");
        return 2;
    }

    append_log("allowed");
    0
}

pub fn parse_payload(raw: &str, process_cwd: &Path) -> Option<HookPayloadView> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    parse_payload_value(&value, process_cwd)
}

pub fn parse_payload_value(value: &Value, process_cwd: &Path) -> Option<HookPayloadView> {
    let object = value.as_object()?;
    let tool_name = object
        .get("tool_name")
        .or_else(|| object.get("toolName"))
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let command = extract_command(value);
    let cwd = object
        .get("cwd")
        .or_else(|| object.get("cwdPath"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| process_cwd.to_path_buf());
    Some(HookPayloadView {
        tool_name,
        command,
        cwd,
    })
}

pub fn forbidden_reason(
    command_text: &str,
    cwd: Option<&Path>,
    bad_commands: &[BadCommandRule],
) -> Option<String> {
    let allow_hybrid_uv_run = std::env::var("CLUD_UV_RUST_ALLOW_ALL").ok().as_deref() == Some("1");
    evaluate_command(command_text, cwd, allow_hybrid_uv_run, bad_commands).reason
}

pub fn decision_from_payload(
    payload: &HookPayloadView,
    bad_commands: &[BadCommandRule],
) -> Decision {
    match forbidden_reason(&payload.command, Some(&payload.cwd), bad_commands) {
        Some(reason) => Decision::Deny { reason },
        None => Decision::Allow,
    }
}

pub fn deny_json(reason: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
}

pub fn evaluate_command(
    command_text: &str,
    cwd: Option<&Path>,
    allow_hybrid_uv_run: bool,
    bad_commands: &[BadCommandRule],
) -> CommandEvaluation {
    let mut evaluation = CommandEvaluation::default();
    evaluate_command_into(
        command_text,
        cwd,
        allow_hybrid_uv_run,
        bad_commands,
        0,
        &mut evaluation,
    );
    evaluation
}

fn evaluate_command_into(
    command_text: &str,
    cwd: Option<&Path>,
    allow_hybrid_uv_run: bool,
    bad_commands: &[BadCommandRule],
    depth: usize,
    evaluation: &mut CommandEvaluation,
) {
    if depth > MAX_SUBSTITUTION_RECURSION_DEPTH {
        evaluation.log_messages.push(format!(
            "substitution recursion depth {depth} exceeds cap {MAX_SUBSTITUTION_RECURSION_DEPTH}; failing open on remainder of command"
        ));
        return;
    }

    if command_text.to_ascii_lowercase().contains(SENTINEL_PHRASE) {
        evaluation.reason = Some(format!(
            "command contains {:?}. Full command: {}",
            SENTINEL_PHRASE,
            py_string_repr(command_text)
        ));
        return;
    }

    let command_text_owned;
    let command_text = if depth == 0 {
        command_text_owned = strip_heredoc_bodies(command_text);
        command_text_owned.as_str()
    } else {
        command_text
    };

    for inner in scan_command_substitutions(command_text) {
        evaluate_command_into(
            &inner,
            cwd,
            allow_hybrid_uv_run,
            bad_commands,
            depth + 1,
            evaluation,
        );
        if evaluation.reason.is_some() {
            return;
        }
    }

    for segment in split_shell_segments(command_text) {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        let words = command_words(segment);
        if words.is_empty() {
            continue;
        }

        let first = program_name(&words[0]);
        if let Some(nested) = nested_shell_command(&words) {
            evaluate_command_into(
                &nested,
                cwd,
                allow_hybrid_uv_run,
                bad_commands,
                depth + 1,
                evaluation,
            );
            if evaluation.reason.is_some() {
                return;
            }
            continue;
        }

        if let Some(reason) = evaluate_generic_rules(&words, bad_commands, evaluation) {
            evaluation.reason = Some(reason);
            return;
        }

        if contains_str(LEGACY_RUST_TRAMPOLINES, &first) {
            evaluation.reason = Some(format!(
                "Use `soldr {} ...` instead of legacy `{}`. The root Rust trampolines bypass soldr's toolchain selection.",
                first.trim_start_matches('_'),
                words[0]
            ));
            return;
        }

        if first == "soldr" {
            continue;
        }

        if first == "uv" && words.len() > 1 && words[1] == "run" {
            if let Some(tool) = resolve_uv_run_tool(&words) {
                let tool_bare = program_name(&tool);
                if contains_str(LEGACY_RUST_TRAMPOLINES, &tool_bare) {
                    evaluation.reason = Some(format!(
                        "Use `soldr {} ...` instead of legacy `{}`. The root Rust trampolines bypass soldr's toolchain selection.",
                        tool_bare.trim_start_matches('_'),
                        tool
                    ));
                    return;
                }
                if contains_str(RUST_TOOLS, &tool_bare) {
                    evaluation.reason = Some(format!(
                        "Use `soldr {tool_bare} ...` instead of `uv run {tool} ...`. `uv run <rust-tool>` bypasses soldr's toolchain selection."
                    ));
                    return;
                }
            }

            let uv_safe_flags = ["--no-project", "--no-sync", "--frozen"];
            let has_uv_safe_flag = words[2..].iter().any(|word| {
                uv_safe_flags
                    .iter()
                    .any(|flag| word == flag || word.starts_with(&format!("{flag}=")))
            });
            if !has_uv_safe_flag {
                if let Some(hybrid_root) = python_rust_hybrid_root(cwd) {
                    if allow_hybrid_uv_run {
                        evaluation.log_messages.push(format!(
                            "CLUD_UV_RUST_ALLOW_ALL=1 bypassed hybrid block at {}",
                            hybrid_root.display()
                        ));
                        evaluation
                            .warnings
                            .push(hybrid_bypass_warning(&hybrid_root));
                    } else {
                        evaluation.reason = Some(format!(
                            "this hook fired because {} contains both pyproject.toml and Cargo.toml (a Python+Rust hybrid project). `uv run` without --no-project / --no-sync / --frozen triggers the project auto-sync, which on a Rust-backed wheel is a full native rebuild. Pass `--no-project` for pure-Python scripts, `--no-sync` to use the existing venv, or `--frozen` to lock to the existing lockfile. Escape hatch for a legitimate full-rebuild: run `./test` (or `bash ./test`) - the canonical full-build entrypoint. Set CLUD_UV_RUST_ALLOW_ALL=1 to bypass this gate with a warning. See zackees/soldr#805.",
                            hybrid_root.display()
                        ));
                        return;
                    }
                }
            }
            continue;
        }

        if contains_str(RUST_TOOLS, &first) {
            evaluation.reason = Some(format!(
                "Use `soldr {first} ...` instead of bare `{first}`. soldr resolves the pinned rustup-managed toolchain and avoids GNU/Chocolatey shims."
            ));
            return;
        }
    }
}

/// Evaluate the repo/user-configured generic `bad_commands` rules
/// against one segment's tokenized `words` (zackees/clud#519). Returns
/// `Some(deny reason)` on the first matching, non-overridden rule.
///
/// Matching is against the normalized program-name token, never the
/// raw command line — this is what makes `rg playwright` /
/// `grep -r playwright .` correctly pass through, since their head
/// token is `rg`/`grep`, not `playwright`.
///
/// `passthrough_prefixes` (soldr-style) is resolved per rule, one
/// token at a time: when the current head token matches a rule's own
/// `passthrough_prefixes`, that rule is permanently excluded from the
/// rest of this segment's evaluation (it does not get re-checked
/// against whatever the prefix wraps) and the scan advances to the
/// next token — but only for the rules that recognized this prefix.
/// Rules that don't declare that prefix keep evaluating against the
/// *unwrapped* head, so `soldr foo run` still trips a `foo` rule that
/// never opted into trusting `soldr` (see
/// `generic_rule_passthrough_does_not_blanket_exempt_other_rules`).
fn evaluate_generic_rules(
    words: &[String],
    bad_commands: &[BadCommandRule],
    evaluation: &mut CommandEvaluation,
) -> Option<String> {
    if words.is_empty() || bad_commands.is_empty() {
        return None;
    }

    let mut active: Vec<usize> = (0..bad_commands.len()).collect();
    let mut idx = 0usize;
    while idx < words.len() && !active.is_empty() {
        let head = program_name(&words[idx]);
        let mut cleared_this_round = Vec::new();
        let mut still_active = Vec::new();
        for &rule_idx in &active {
            let rule = &bad_commands[rule_idx];
            if let Some(matched_prefix) =
                passthrough_prefix_match(&rule.passthrough_prefixes, rule.match_mode, &head)
            {
                let rule_label = rule.id.as_deref().unwrap_or(rule.pattern.as_str());
                evaluation.log_messages.push(format!(
                    "BAD_CMD_PASSTHROUGH rule={rule_label} prefix={matched_prefix:?} matched_token={head:?} command={:?}",
                    words.join(" ")
                ));
                cleared_this_round.push(rule_idx);
            } else {
                still_active.push(rule_idx);
            }
        }

        for &rule_idx in &still_active {
            let rule = &bad_commands[rule_idx];
            let compiled = match compile_match_pattern(&rule.pattern, rule.match_mode) {
                Ok(re) => re,
                // Rules are validated at config-parse time; this should
                // be unreachable, but skip rather than abort the whole
                // evaluation if it ever happens.
                Err(_) => continue,
            };
            if !compiled.is_match(&head) {
                continue;
            }

            if rule.allow_override {
                if let Some(id) = &rule.id {
                    if let Some(override_reason) = accepted_override_reason(id) {
                        evaluation.log_messages.push(format!(
                            "BAD_CMD_OVERRIDE accepted rule={id} reason={override_reason:?} command={:?}",
                            words.join(" ")
                        ));
                        continue;
                    }
                }
            }

            let reason = if rule.reason.is_empty() {
                format!("`{head}` is a blocked command.")
            } else {
                rule.reason.clone()
            };
            let mut message = format!("{reason} Use `{}` instead.", rule.replacement);
            if rule.allow_override {
                if let Some(id) = &rule.id {
                    message.push_str(&format!(
                        " To intentionally bypass this rule for this one command, set the real environment variable {BAD_CMD_OVERRIDE_ENV}=\"{id}:<your reason for needing the raw command>\" for this tool call (not text prepended to the command itself) and re-run the exact same command unchanged."
                    ));
                }
            }
            return Some(message);
        }

        // `soldr` is a universally-trusted transparent wrapper (mirrors
        // the hardcoded `if first == "soldr" { continue; }` fast path
        // for RUST_TOOLS below) — the scan advances past it regardless
        // of whether any individual rule happens to list it in its own
        // `passthrough_prefixes`. That field only controls whether a
        // *specific* rule is exempted from firing on the wrapper token
        // itself; it must not gate whether *other* rules get to look
        // past it, or `soldr <bad-program>` would be allowed for any
        // rule that never explicitly opted into trusting soldr.
        if cleared_this_round.is_empty() && !head.eq_ignore_ascii_case("soldr") {
            break;
        }
        active = still_active;
        idx += 1;
    }
    None
}

/// `passthrough_prefixes` entries are patterns in the *same*
/// `match_mode` as the rule's own `match` field — glob or regex for
/// the whole list, never mixed per-entry, quoted like any other JSON
/// string (e.g. `["soldr"]` or, in regex mode, `["^soldr(-\\w+)?$"]`).
/// Returns the specific prefix pattern that matched, for logging.
fn passthrough_prefix_match<'a>(
    prefixes: &'a [String],
    mode: MatchMode,
    head: &str,
) -> Option<&'a str> {
    prefixes.iter().find_map(|prefix| {
        let is_match = compile_match_pattern(prefix, mode)
            .map(|re| re.is_match(head))
            .unwrap_or_else(|_| prefix.eq_ignore_ascii_case(head));
        is_match.then_some(prefix.as_str())
    })
}

/// Check the real process environment (never the command text — see
/// the module-level `BAD_CMD_OVERRIDE_ENV` doc comment) for an
/// override matching `rule_id`, with a mandatory non-empty reason.
/// Returns the reason string on an accepted override.
fn accepted_override_reason(rule_id: &str) -> Option<String> {
    let raw = std::env::var(BAD_CMD_OVERRIDE_ENV).ok()?;
    let (override_id, reason) = raw.split_once(':')?;
    let reason = reason.trim();
    if override_id == rule_id && !reason.is_empty() {
        Some(reason.to_string())
    } else {
        None
    }
}

/// Detect and strip heredoc bodies (`<<'DELIM'`, `<<DELIM`, `<<-DELIM`)
/// from `text` so their contents are never scanned as commands — a
/// heredoc body is data piped to the receiving command, not executed.
/// Deliberately does not touch `<<<` here-strings (single-line, never
/// span multiple lines, so segment-splitting already treats them as
/// plain argument text).
fn strip_heredoc_bodies(text: &str) -> String {
    if !text.contains("<<") {
        return text.to_string();
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out_lines: Vec<&str> = Vec::with_capacity(lines.len());
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        out_lines.push(line);
        if let Some(delim) = find_heredoc_delimiter(line) {
            let body_start = i + 1;
            let mut j = body_start;
            let mut terminator_index = None;
            while j < lines.len() {
                // Trim a trailing '\r' too: `text` may have originated
                // from a CRLF payload split on '\n' alone, leaving a
                // stray '\r' that would otherwise make a real
                // terminator line fail to match `delim`.
                let body_line = lines[j].trim_start_matches('\t').trim_end_matches('\r');
                if body_line == delim {
                    terminator_index = Some(j);
                    break;
                }
                j += 1;
            }
            match terminator_index {
                Some(terminator_index) => {
                    // Skip the body lines (never scanned as commands)
                    // and the terminator line itself.
                    i = terminator_index + 1;
                }
                None => {
                    // No matching terminator found (malformed/adversarial
                    // input, e.g. a mismatched delimiter). Fail toward
                    // *more* scanning, not less: keep every line from
                    // here on in the output rather than silently
                    // dropping real trailing commands unscanned.
                    out_lines.extend_from_slice(&lines[body_start..]);
                    i = lines.len();
                }
            }
            continue;
        }
        i += 1;
    }
    out_lines.join("\n")
}

fn find_heredoc_delimiter(line: &str) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut idx = 0usize;
    let mut quote: Option<char> = None;
    let mut arithmetic_depth = 0i32;
    while idx + 1 < chars.len() {
        let c = chars[idx];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            idx += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            idx += 1;
            continue;
        }
        // `$((...))` arithmetic expansion: `<<` inside it is the
        // left-shift operator, never a heredoc redirection. Track
        // depth via the paren-balance already used for `$(...)`
        // elsewhere; here we only need to know "inside or not" per
        // line, so a simple depth counter on `((`/`))` suffices.
        if c == '$' && idx + 2 < chars.len() && chars[idx + 1] == '(' && chars[idx + 2] == '(' {
            arithmetic_depth += 1;
            idx += 3;
            continue;
        }
        if arithmetic_depth > 0 {
            if c == '(' {
                arithmetic_depth += 1;
            } else if c == ')' {
                arithmetic_depth -= 1;
            }
            idx += 1;
            continue;
        }
        if c == '<' && chars[idx + 1] == '<' {
            // exclude here-strings (`<<<`), which are single-line data.
            if idx + 2 < chars.len() && chars[idx + 2] == '<' {
                idx += 1;
                continue;
            }
            let mut j = idx + 2;
            if j < chars.len() && chars[j] == '-' {
                j += 1;
            }
            while j < chars.len() && chars[j] == ' ' {
                j += 1;
            }
            let delim_quote = if j < chars.len() && (chars[j] == '\'' || chars[j] == '"') {
                let q = chars[j];
                j += 1;
                Some(q)
            } else {
                None
            };
            let start = j;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            if j == start {
                idx += 1;
                continue;
            }
            let delimiter: String = chars[start..j].iter().collect();
            let _ = delim_quote;
            return Some(delimiter);
        }
        idx += 1;
    }
    None
}

/// Extract the inner text of every command-substitution / subshell /
/// process-substitution span in `text` — backticks, `$(...)`
/// (excluding `$((...))` arithmetic expansion), and `<(...)`/`>(...)`
/// process substitution — for recursive evaluation. Bare `(...)`
/// subshell grouping in command position is already handled by the
/// per-segment scan treating `(` as an ordinary token boundary once
/// tokenized, so it is not duplicated here.
fn scan_command_substitutions(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        match chars[i] {
            '`' => {
                let start = i + 1;
                let mut j = start;
                let mut escaped = false;
                while j < chars.len() {
                    if escaped {
                        escaped = false;
                        j += 1;
                        continue;
                    }
                    if chars[j] == '\\' {
                        escaped = true;
                        j += 1;
                        continue;
                    }
                    if chars[j] == '`' {
                        break;
                    }
                    j += 1;
                }
                if j < chars.len() {
                    spans.push(chars[start..j].iter().collect());
                    i = j + 1;
                } else {
                    i = chars.len();
                }
            }
            '$' if i + 1 < chars.len() && chars[i + 1] == '(' => {
                if i + 2 < chars.len() && chars[i + 2] == '(' {
                    // Arithmetic expansion $((...)) — not a command;
                    // skip past its matching `))` without recursing.
                    if let Some(end) = find_matching_double_paren_close(&chars, i + 2) {
                        i = end + 1;
                    } else {
                        i += 1;
                    }
                } else if let Some((inner, end)) = extract_paren_balanced(&chars, i + 1) {
                    spans.push(inner);
                    i = end;
                } else {
                    i += 1;
                }
            }
            '<' | '>' if i + 1 < chars.len() && chars[i + 1] == '(' => {
                if let Some((inner, end)) = extract_paren_balanced(&chars, i + 1) {
                    spans.push(inner);
                    i = end;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    spans
}

/// `chars[open]` must be `(`. Returns (inner text, index just past the
/// matching close paren), tracking nested-paren depth. Ignores quotes
/// inside the span (acceptable simplification for this hot-path scan).
fn extract_paren_balanced(chars: &[char], open: usize) -> Option<(String, usize)> {
    let mut depth = 0i32;
    let mut j = open;
    while j < chars.len() {
        match chars[j] {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((chars[open + 1..j].iter().collect(), j + 1));
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// `chars[open]` must be the first `(` of a `$((` arithmetic-expansion
/// opener. Returns the index of the final closing `)` of the matching
/// `))`, tracking nested-paren depth starting at 2 (for the doubled
/// open).
fn find_matching_double_paren_close(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut j = open;
    while j < chars.len() {
        match chars[j] {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

fn extract_command(payload: &Value) -> String {
    let Some(object) = payload.as_object() else {
        return String::new();
    };
    let Some(tool_input) = object.get("tool_input").or_else(|| object.get("toolInput")) else {
        return String::new();
    };
    if let Some(map) = tool_input.as_object() {
        for key in ["command", "script"] {
            if let Some(command) = map.get(key).and_then(Value::as_str) {
                return command.to_string();
            }
        }
        if let Some(argv) = map.get("argv").and_then(Value::as_array) {
            return argv
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(ToOwned::to_owned)
                        .unwrap_or_else(|| value.to_string())
                })
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
    tool_input.as_str().unwrap_or("").to_string()
}

fn split_shell_segments(command_text: &str) -> Vec<String> {
    let chars = command_text.chars().collect::<Vec<_>>();
    let mut segments = Vec::new();
    let mut buf = String::new();
    let mut quote: Option<char> = None;
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if let Some(q) = quote {
            buf.push(ch);
            if ch == q {
                quote = None;
            }
            i += 1;
            continue;
        }

        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            buf.push(ch);
            i += 1;
            continue;
        }

        let is_double_amp = ch == '&' && i + 1 < chars.len() && chars[i + 1] == '&';
        let is_double_pipe = ch == '|' && i + 1 < chars.len() && chars[i + 1] == '|';
        if matches!(ch, ';' | '|' | '\r' | '\n') || is_double_amp {
            let segment = buf.trim();
            if !segment.is_empty() {
                segments.push(segment.to_string());
            }
            buf.clear();
            i += if is_double_amp || is_double_pipe {
                2
            } else {
                1
            };
            continue;
        }

        buf.push(ch);
        i += 1;
    }

    let segment = buf.trim();
    if !segment.is_empty() {
        segments.push(segment.to_string());
    }
    segments
}

fn tokenize(segment: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut buf = String::new();
    let mut quote: Option<char> = None;
    for ch in segment.chars() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                buf.push(ch);
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            continue;
        }
        if ch.is_whitespace() {
            if !buf.is_empty() {
                words.push(std::mem::take(&mut buf));
            }
            continue;
        }
        buf.push(ch);
    }
    if !buf.is_empty() {
        words.push(buf);
    }
    words
}

fn program_name(word: &str) -> String {
    let cleaned = word.trim().trim_matches(&['\'', '"'][..]);
    crate::path_norm::file_stem_any_separator(cleaned)
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn command_words(segment: &str) -> Vec<String> {
    let mut words = tokenize(segment);
    while words
        .first()
        .is_some_and(|word| ["&", "call", "exec", "command"].contains(&word.as_str()))
    {
        words.remove(0);
    }
    if words
        .first()
        .is_some_and(|word| program_name(word) == "env")
    {
        words.remove(0);
    }
    while words.first().is_some_and(|word| is_env_assignment(word)) {
        words.remove(0);
    }
    words
}

fn is_env_assignment(word: &str) -> bool {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    for ch in chars {
        if ch == '=' {
            return true;
        }
        if !(ch == '_' || ch.is_ascii_alphanumeric()) {
            return false;
        }
    }
    false
}

fn resolve_uv_run_tool(words: &[String]) -> Option<String> {
    if words.len() < 3 || program_name(&words[0]) != "uv" || words[1] != "run" {
        return None;
    }
    let mut i = 2usize;
    while i < words.len() {
        let word = &words[i];
        if word == "--" {
            i += 1;
            break;
        }
        if word == "--script" && i + 1 < words.len() {
            return Some(words[i + 1].clone());
        }
        if let Some(value) = word.strip_prefix("--script=") {
            return Some(value.to_string());
        }
        if !word.starts_with('-') {
            break;
        }
        let consumes_value = (!word.contains('=') && contains_str(UV_RUN_OPTIONS_WITH_VALUE, word))
            || contains_str(UV_RUN_SHORT_OPTIONS_WITH_VALUE, word);
        if consumes_value {
            i += 2;
        } else {
            i += 1;
        }
    }
    words.get(i).cloned()
}

fn nested_shell_command(words: &[String]) -> Option<String> {
    let first = program_name(words.first()?);
    if !contains_str(SHELL_WRAPPERS, &first) {
        return None;
    }
    if first == "eval" {
        if words.len() > 1 {
            return Some(words[1..].join(" "));
        }
        return None;
    }
    if first == "cmd" {
        for (i, word) in words.iter().enumerate().skip(1) {
            if ["/c", "/k", "/r"].contains(&word.to_ascii_lowercase().as_str())
                && i + 1 < words.len()
            {
                return Some(words[i + 1..].join(" "));
            }
        }
        return None;
    }
    if first == "powershell" || first == "pwsh" {
        for (i, word) in words.iter().enumerate().skip(1) {
            if ["-command", "-c", "/c"].contains(&word.to_ascii_lowercase().as_str())
                && i + 1 < words.len()
            {
                return Some(words[i + 1..].join(" "));
            }
        }
        return None;
    }

    for (i, word) in words.iter().enumerate().skip(1) {
        let option = word.to_ascii_lowercase();
        let option = option.trim_start_matches('-');
        if option.contains('c') && i + 1 < words.len() {
            return Some(words[i + 1..].join(" "));
        }
    }
    None
}

fn python_rust_hybrid_root(cwd: Option<&Path>) -> Option<PathBuf> {
    let anchor = cwd?.canonicalize().ok()?;
    for candidate in std::iter::once(anchor.as_path()).chain(anchor.ancestors().skip(1)) {
        if candidate.join("pyproject.toml").is_file() && candidate.join("Cargo.toml").is_file() {
            return Some(candidate.to_path_buf());
        }
    }
    None
}

fn hybrid_bypass_warning(hybrid_root: &Path) -> String {
    format!(
        "\x1b[33mWARNING: AUTO COMPILING RUST because of uv run\n\
CLUD_UV_RUST_ALLOW_ALL=1 is set, so the auto-sync gate at {} was bypassed.\n\
DIRECTIVE TO AGENT: the next `uv run` in this project root will trigger a full native rebuild (can take minutes). \
If you don't need a fresh build, pass `--no-sync` (use existing venv), `--no-project` (pure-Python script), or \
`--frozen` (lock to existing lockfile) to skip the auto-sync. If you DO need a clean rebuild, prefer `./test` \
(or `bash ./test`) - the canonical full-build entrypoint.\x1b[0m",
        hybrid_root.display()
    )
}

fn contains_str(haystack: &[&str], needle: &str) -> bool {
    haystack.iter().any(|item| item == &needle)
}

fn py_string_repr(value: &str) -> String {
    let mut out = String::from("'");
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('\'');
    out
}

fn read_stdin_bounded() -> StdinRead {
    #[cfg(unix)]
    {
        if let Some(read) = read_stdin_nonblocking() {
            return read;
        }
    }
    read_stdin_threaded()
}

#[cfg(unix)]
fn read_stdin_nonblocking() -> Option<StdinRead> {
    use std::os::fd::AsRawFd;

    let idle_timeout = float_env_duration(
        "CLUD_HOOK_STDIN_IDLE_TIMEOUT_SEC",
        DEFAULT_STDIN_READ_IDLE_TIMEOUT_SEC,
    );
    let deadline_timeout = float_env_duration(
        "CLUD_HOOK_STDIN_DEADLINE_SEC",
        DEFAULT_STDIN_READ_DEADLINE_SEC,
    );

    let stdin = io::stdin();
    let mut stream = stdin.lock();
    let fd = stream.as_raw_fd();
    let old_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if old_flags < 0 {
        return None;
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, old_flags | libc::O_NONBLOCK) } < 0 {
        return None;
    }

    let mut chunks = Vec::<u8>::new();
    let mut log_messages = Vec::<String>::new();
    let deadline = Instant::now() + deadline_timeout;
    let mut idle_until: Option<Instant> = None;
    let mut incomplete_reason: Option<&'static str> = None;
    loop {
        let mut buf = [0u8; STDIN_READ_CHUNK_BYTES];
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                chunks.extend_from_slice(&buf[..n]);
                idle_until = Some(Instant::now() + idle_timeout);
                if chunks.len() >= STDIN_READ_MAX_BYTES {
                    incomplete_reason = Some("max_bytes");
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let now = Instant::now();
                let wait_until = idle_until.map_or(deadline, |idle| idle.min(deadline));
                if now >= wait_until {
                    incomplete_reason = Some(if idle_until.is_some() && wait_until <= deadline {
                        "idle"
                    } else {
                        "deadline"
                    });
                    break;
                }
                std::thread::sleep((wait_until - now).min(Duration::from_millis(10)));
            }
            Err(error) => {
                log_messages.push(format!("stdin_read_error mode=nonblocking error={error}"));
                break;
            }
        }
    }

    let _ = unsafe { libc::fcntl(fd, libc::F_SETFL, old_flags) };
    if let Some(reason) = incomplete_reason {
        log_messages.push(format!(
            "stdin_read_incomplete mode=nonblocking reason={reason} bytes={}",
            chunks.len()
        ));
    }
    Some(StdinRead {
        text: decode_stdin(&chunks),
        log_messages,
    })
}

fn read_stdin_threaded() -> StdinRead {
    enum Item {
        Chunk(Vec<u8>),
        Eof,
        Error(String),
    }

    let idle_timeout = float_env_duration(
        "CLUD_HOOK_STDIN_IDLE_TIMEOUT_SEC",
        DEFAULT_STDIN_READ_IDLE_TIMEOUT_SEC,
    );
    let deadline_timeout = float_env_duration(
        "CLUD_HOOK_STDIN_DEADLINE_SEC",
        DEFAULT_STDIN_READ_DEADLINE_SEC,
    );
    let (tx, rx) = mpsc::channel::<Item>();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut stream = stdin.lock();
        loop {
            let mut buf = vec![0u8; STDIN_READ_CHUNK_BYTES];
            match stream.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(Item::Eof);
                    return;
                }
                Ok(n) => {
                    buf.truncate(n);
                    if tx.send(Item::Chunk(buf)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = tx.send(Item::Error(error.to_string()));
                    return;
                }
            }
        }
    });

    let mut chunks = Vec::<u8>::new();
    let mut log_messages = Vec::<String>::new();
    let deadline = Instant::now() + deadline_timeout;
    let mut idle_until: Option<Instant> = None;
    let mut incomplete_reason: Option<&'static str> = None;
    loop {
        let now = Instant::now();
        let wait_until = idle_until.map_or(deadline, |idle| idle.min(deadline));
        if now >= wait_until {
            incomplete_reason = Some(if idle_until.is_some() && wait_until <= deadline {
                "idle"
            } else {
                "deadline"
            });
            break;
        }
        match rx.recv_timeout(wait_until - now) {
            Ok(Item::Eof) => break,
            Ok(Item::Error(error)) => {
                log_messages.push(format!("stdin_read_error mode=threaded error={error}"));
                break;
            }
            Ok(Item::Chunk(chunk)) => {
                chunks.extend_from_slice(&chunk);
                idle_until = Some(Instant::now() + idle_timeout);
                if chunks.len() >= STDIN_READ_MAX_BYTES {
                    incomplete_reason = Some("max_bytes");
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                incomplete_reason = Some(if idle_until.is_some() && wait_until <= deadline {
                    "idle"
                } else {
                    "deadline"
                });
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    if let Some(reason) = incomplete_reason {
        log_messages.push(format!(
            "stdin_read_incomplete mode=threaded reason={reason} bytes={}",
            chunks.len()
        ));
    }
    StdinRead {
        text: decode_stdin(&chunks),
        log_messages,
    }
}

fn decode_stdin(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_start_matches('\u{feff}')
        .to_string()
}

fn float_env_duration(name: &str, default: f64) -> Duration {
    let seconds = std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<f64>().ok())
        .unwrap_or(default)
        .max(0.01);
    Duration::from_secs_f64(seconds)
}

pub fn log_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(LOG_REL_PATH))
}

fn append_log(message: &str) {
    let Some(path) = log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string());
    let _ = writeln!(file, "[{timestamp}] pid={} {message}", std::process::id());
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(path) = std::env::var_os("USERPROFILE") {
            if !path.to_string_lossy().is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }
    std::env::var_os("HOME")
        .filter(|path| !path.to_string_lossy().is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn denies(command: &str) -> bool {
        evaluate_command(command, None, false, &[]).reason.is_some()
    }

    fn allows(command: &str) -> bool {
        !denies(command)
    }

    fn denies_with_rules(command: &str, rules: &[BadCommandRule]) -> bool {
        evaluate_command(command, None, false, rules)
            .reason
            .is_some()
    }

    fn allows_with_rules(command: &str, rules: &[BadCommandRule]) -> bool {
        !denies_with_rules(command, rules)
    }

    fn eval_with_rules(command: &str, rules: &[BadCommandRule]) -> CommandEvaluation {
        evaluate_command(command, None, false, rules)
    }

    // `allow_override: false` by default: only the override-specific
    // tests below need it true, and they always run through
    // `temp_env`'s mutex. Rules that never consult `allow_override`
    // are immune to the process-global `CLUD_BAD_CMD_OVERRIDE` env var
    // that those tests set concurrently on other test threads.
    fn playwright_rule() -> BadCommandRule {
        BadCommandRule {
            id: Some("no-raw-playwright".to_string()),
            pattern: "playwright".to_string(),
            match_mode: MatchMode::Glob,
            replacement: "npm run test:integration".to_string(),
            reason: "use the blessed pipeline; raw playwright is slower".to_string(),
            passthrough_prefixes: vec!["soldr".to_string()],
            allow_override: false,
        }
    }

    fn playwright_rule_overridable() -> BadCommandRule {
        BadCommandRule {
            allow_override: true,
            ..playwright_rule()
        }
    }

    #[test]
    fn sentinel_phrase_denies() {
        let command = concat!("echo ", "bad", " cmd");
        let reason = evaluate_command(command, None, false, &[]).reason.unwrap();
        assert!(reason.contains(SENTINEL_PHRASE));
    }

    #[test]
    fn blocks_bare_rust_tools() {
        for tool in RUST_TOOLS {
            assert!(
                denies(&format!("{tool} --version")),
                "{tool} should be denied"
            );
            assert!(
                denies(&format!("C:/tools/{tool}.exe --version")),
                "{tool}.exe should be denied"
            );
            assert!(
                denies(&format!(r"C:\tools\{tool}.cmd --version")),
                "{tool}.cmd should be denied"
            );
        }
    }

    #[test]
    fn allows_soldr_prefixed_rust_tools() {
        assert!(allows(&format!("soldr {TOOL_RS_BUILD} build")));
        assert!(allows(&format!(
            "echo before && soldr {TOOL_RS_COMPILER} --version"
        )));
    }

    #[test]
    fn env_prefixed_rust_tools_are_denied() {
        assert!(denies(&format!("FOO=bar {TOOL_RS_BUILD} build")));
        assert!(denies(&format!("env FOO=bar {TOOL_RS_BUILD} build")));
    }

    #[test]
    fn legacy_trampolines_are_denied() {
        for tool in LEGACY_RUST_TRAMPOLINES {
            assert!(denies(&format!("{tool} build")), "{tool} should be denied");
            assert!(
                denies(&format!("uv run {tool} build")),
                "uv run {tool} should be denied"
            );
        }
    }

    #[test]
    fn uv_run_rust_tools_are_denied() {
        assert!(denies(&format!("uv run {TOOL_RS_BUILD} test")));
        assert!(denies(&format!("uv run --with foo {TOOL_RS_BUILD} test")));
        assert!(denies(&format!("uv run --no-sync {TOOL_RS_BUILD} test")));
        assert!(denies(&format!("uv run --no-project {TOOL_RS_BUILD} test")));
        assert!(denies(&format!(
            "uv run --frozen {TOOL_RS_COMPILER} --version"
        )));
        assert!(denies(&format!("uv run --no-binary {TOOL_RS_BUILD} test")));
        assert!(denies(&format!(
            "uv run --with=foo {TOOL_RS_COMPILER} --version"
        )));
        assert!(allows(&format!("uv run --with {TOOL_RS_BUILD} python -V")));
        assert!(allows(&format!("uv run -w {TOOL_RS_BUILD} python -V")));
        assert!(allows(&format!("uv run -m {TOOL_RS_BUILD}")));
        assert!(allows("uv run --script some.py"));
        assert!(allows("uv run --script=some.py"));
    }

    #[test]
    fn nested_shell_wrappers_are_denied() {
        for command in [
            format!("cmd /c {TOOL_RS_BUILD} build"),
            format!("powershell -Command {TOOL_RS_BUILD} build"),
            format!("pwsh -c {TOOL_RS_BUILD} build"),
            format!("bash -c '{TOOL_RS_BUILD} build'"),
            format!("sh -c '{TOOL_RS_BUILD} build'"),
        ] {
            assert!(denies(&command), "{command} should be denied");
        }
    }

    #[test]
    fn quoted_mentions_are_not_invocations() {
        assert!(allows(&format!("echo '{TOOL_RS_BUILD} build'")));
        assert!(allows(&format!("printf \"{TOOL_RS_COMPILER}\"")));
    }

    #[test]
    fn shell_segments_are_scanned_independently() {
        assert!(denies(&format!("echo ok; {TOOL_RS_BUILD} build")));
        assert!(denies(&format!("echo ok && {TOOL_RS_COMPILER} --version")));
        assert!(denies(&format!("echo ok || {TOOL_RS_FORMAT} --version")));
        assert!(allows(&format!("echo 'ok && {TOOL_RS_BUILD} build'")));
    }

    #[test]
    fn hybrid_uv_run_blocks_only_polyglot_roots_without_safe_flags() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("pyproject.toml"), "[project]\nname='x'\n").unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        let nested = root.join("a/b");
        std::fs::create_dir_all(&nested).unwrap();

        assert!(
            evaluate_command("uv run python -V", Some(&nested), false, &[])
                .reason
                .is_some()
        );
        assert!(
            evaluate_command("uv run --no-sync python -V", Some(&nested), false, &[])
                .reason
                .is_none()
        );
        assert!(
            evaluate_command("uv run --no-project python -V", Some(&nested), false, &[])
                .reason
                .is_none()
        );
        assert!(
            evaluate_command("uv run --frozen python -V", Some(&nested), false, &[])
                .reason
                .is_none()
        );
    }

    #[test]
    fn hybrid_uv_run_allow_all_bypasses_only_hybrid_auto_sync_case() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("pyproject.toml"), "[project]\nname='x'\n").unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();

        let allowed = evaluate_command("uv run python -V", Some(root), true, &[]);
        assert!(allowed.reason.is_none());
        assert_eq!(allowed.warnings.len(), 1);
        assert!(
            evaluate_command(
                &format!("uv run {TOOL_RS_BUILD} test"),
                Some(root),
                true,
                &[]
            )
            .reason
            .is_some(),
            "bypass must not allow direct Rust tool execution"
        );
    }

    #[test]
    fn pure_python_or_pure_rust_roots_do_not_trigger_hybrid_block() {
        let py = tempdir().unwrap();
        std::fs::write(py.path().join("pyproject.toml"), "[project]\nname='x'\n").unwrap();
        assert!(
            evaluate_command("uv run python -V", Some(py.path()), false, &[])
                .reason
                .is_none()
        );

        let rs = tempdir().unwrap();
        std::fs::write(rs.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        assert!(
            evaluate_command("uv run python -V", Some(rs.path()), false, &[])
                .reason
                .is_none()
        );
    }

    #[test]
    fn payload_aliases_are_supported() {
        let cwd = PathBuf::from("repo");
        let payload = format!(
            r#"{{"toolName":"Shell","toolInput":{{"argv":["{}","test"]}},"cwdPath":"repo"}}"#,
            TOOL_RS_BUILD
        );
        let parsed = parse_payload(&payload, Path::new(".")).unwrap();
        assert_eq!(parsed.tool_name, "Shell");
        assert_eq!(parsed.command, format!("{TOOL_RS_BUILD} test"));
        assert_eq!(parsed.cwd, cwd);
        assert!(matches!(
            decision_from_payload(&parsed, &[]),
            Decision::Deny { .. }
        ));
    }

    #[test]
    fn deny_json_matches_hook_contract() {
        let value = deny_json("nope");
        assert_eq!(
            value["hookSpecificOutput"]["hookEventName"],
            Value::String("PreToolUse".to_string())
        );
        assert_eq!(
            value["hookSpecificOutput"]["permissionDecision"],
            Value::String("deny".to_string())
        );
        assert_eq!(
            value["hookSpecificOutput"]["permissionDecisionReason"],
            Value::String("nope".to_string())
        );
    }

    // -----------------------------------------------------------------
    // Generic `bad_commands` rules (zackees/clud#519).
    // -----------------------------------------------------------------

    #[test]
    fn generic_rule_blocks_bare_invocation() {
        let rules = [playwright_rule()];
        assert!(denies_with_rules("playwright run", &rules));
        let reason = eval_with_rules("playwright run", &rules).reason.unwrap();
        assert!(reason.contains("npm run test:integration"));
    }

    #[test]
    fn generic_rule_allows_unrelated_commands() {
        let rules = [playwright_rule()];
        assert!(allows_with_rules("npm run test:integration", &rules));
        assert!(allows_with_rules("npm test", &rules));
    }

    #[test]
    fn generic_rule_does_not_match_as_argument_ripgrep() {
        let rules = [playwright_rule()];
        assert!(allows_with_rules("rg playwright", &rules));
        assert!(allows_with_rules("grep -r playwright .", &rules));
        assert!(allows_with_rules("ag playwright src/", &rules));
        assert!(allows_with_rules("ack playwright", &rules));
        assert!(allows_with_rules("git grep playwright", &rules));
        assert!(allows_with_rules("git log --grep=playwright", &rules));
        assert!(allows_with_rules("findstr playwright *.ts", &rules));
        assert!(allows_with_rules(
            "gh issue list --search \"playwright\"",
            &rules
        ));
        assert!(allows_with_rules(
            "gh pr create --title \"fix playwright config\"",
            &rules
        ));
    }

    #[test]
    fn generic_rule_does_not_match_quoted_mention() {
        let rules = [playwright_rule()];
        assert!(allows_with_rules(r#"echo "playwright run""#, &rules));
        assert!(allows_with_rules("echo 'run playwright later'", &rules));
        assert!(allows_with_rules(
            r#"echo "TODO: migrate off playwright""#,
            &rules
        ));
    }

    #[test]
    fn generic_rule_does_not_match_path_or_data_arguments() {
        let rules = [playwright_rule()];
        assert!(allows_with_rules("ls playwright-report/", &rules));
        assert!(allows_with_rules("cat playwright.config.ts", &rules));
        assert!(allows_with_rules("rm -rf playwright-report", &rules));
        assert!(allows_with_rules(
            "sed -i 's/playwright/npm run test:integration/' README.md",
            &rules
        ));
        assert!(allows_with_rules(
            "curl https://example.com/playwright/report.json",
            &rules
        ));
    }

    #[test]
    fn generic_rule_case_and_path_normalized() {
        let rules = [playwright_rule()];
        assert!(denies_with_rules("C:/tools/playwright.exe run", &rules));
        assert!(denies_with_rules(r"C:\tools\playwright.cmd run", &rules));
        assert!(denies_with_rules("PLAYWRIGHT run", &rules));
    }

    #[test]
    fn generic_rule_cd_then_replacement_allowed_but_bad_invocation_denied() {
        let rules = [playwright_rule()];
        assert!(allows_with_rules("cd playwright-tests", &rules));
        assert!(allows_with_rules(
            "cd playwright-tests && npm run test:integration",
            &rules
        ));
        assert!(denies_with_rules(
            "cd playwright-tests && playwright run",
            &rules
        ));
    }

    #[test]
    fn generic_rule_chaining_semicolon_and_double_amp_and_pipe() {
        let rules = [playwright_rule()];
        assert!(denies_with_rules("echo hello; playwright run", &rules));
        assert!(denies_with_rules("echo hello && playwright run", &rules));
        assert!(denies_with_rules("echo hello || playwright run", &rules));
        assert!(denies_with_rules(
            "find . -name '*.spec.ts' | playwright run",
            &rules
        ));
        assert!(allows_with_rules(
            r#"echo "hello && playwright run""#,
            &rules
        ));
    }

    #[test]
    fn generic_rule_denied_inside_nested_shell_wrappers() {
        let rules = [playwright_rule()];
        assert!(denies_with_rules("bash -c 'playwright run'", &rules));
        assert!(denies_with_rules(r#"sh -c 'playwright run'"#, &rules));
        assert!(denies_with_rules("zsh -c 'playwright run'", &rules));
        assert!(denies_with_rules(
            r#"powershell -Command "playwright run""#,
            &rules
        ));
        assert!(denies_with_rules(
            r#"powershell.exe -Command "playwright run""#,
            &rules
        ));
        assert!(denies_with_rules(r#"pwsh -c "playwright run""#, &rules));
        assert!(denies_with_rules("cmd.exe /c playwright run", &rules));
        assert!(denies_with_rules(
            r#"bash -c "bash -c 'playwright run'""#,
            &rules
        ));
    }

    #[test]
    fn generic_rule_denied_cmd_slash_k_variant() {
        let rules = [playwright_rule()];
        assert!(denies_with_rules("cmd /k playwright run", &rules));
        assert!(denies_with_rules("cmd.exe /k playwright run", &rules));
    }

    #[test]
    fn generic_rule_denied_with_env_prefix() {
        let rules = [playwright_rule()];
        assert!(denies_with_rules("FOO=bar playwright run", &rules));
        assert!(denies_with_rules("env FOO=bar playwright run", &rules));
    }

    #[test]
    fn generic_rule_denied_inside_command_substitution() {
        let rules = [playwright_rule()];
        assert!(denies_with_rules(r#"echo "$(playwright run)""#, &rules));
        assert!(denies_with_rules("echo $(playwright run)", &rules));
        assert!(denies_with_rules("echo `playwright run`", &rules));
        assert!(denies_with_rules(
            "diff <(playwright run) expected.txt",
            &rules
        ));
        assert!(denies_with_rules("tee >(playwright run)", &rules));
    }

    #[test]
    fn generic_rule_allowed_inside_arithmetic_expansion() {
        let rules = [playwright_rule()];
        assert!(allows_with_rules(r#"echo "$((1 + 2))""#, &rules));
        assert!(allows_with_rules("echo $((3 * 4))", &rules));
        assert!(allows_with_rules(r#"echo "$(( (1 + 2) * 3 ))""#, &rules));
    }

    #[test]
    fn generic_rule_denied_dollar_paren_adjacent_to_arithmetic() {
        let rules = [playwright_rule()];
        assert!(denies_with_rules(
            r#"echo "$(playwright run)$((1+2))""#,
            &rules
        ));
    }

    #[test]
    fn generic_rule_denied_via_eval() {
        let rules = [playwright_rule()];
        assert!(denies_with_rules(r#"eval "playwright run""#, &rules));
        assert!(denies_with_rules("eval 'playwright run'", &rules));
    }

    #[test]
    fn generic_rule_recursion_depth_capped_allows_and_logs() {
        let rules = [playwright_rule()];
        let mut command = "playwright run".to_string();
        for _ in 0..(MAX_SUBSTITUTION_RECURSION_DEPTH + 2) {
            command = format!("echo $({command})");
        }
        let result = eval_with_rules(&command, &rules);
        assert!(result.reason.is_none(), "must fail open past the cap");
        assert!(result
            .log_messages
            .iter()
            .any(|m| m.contains("recursion depth")));
    }

    #[test]
    fn generic_rule_recursion_pathological_depth_no_stack_overflow() {
        let rules = [playwright_rule()];
        let mut command = "echo hi".to_string();
        for _ in 0..2000 {
            command = format!("$({command})");
        }
        let start = Instant::now();
        let _ = eval_with_rules(&command, &rules);
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn generic_rule_heredoc_body_not_scanned() {
        let rules = [playwright_rule()];
        assert!(allows_with_rules(
            "cat <<'EOF'\nplaywright run\nEOF",
            &rules
        ));
        assert!(allows_with_rules("cat <<EOF\nplaywright run\nEOF", &rules));
    }

    #[test]
    fn generic_rule_heredoc_terminator_survives_crlf_payload() {
        // A payload that originated with CRLF line endings but was split
        // on '\n' alone would otherwise leave a stray '\r' on the
        // terminator line, making it fail to match `delim` and (before
        // the fix) silently drop every line after it from scanning.
        let rules = [playwright_rule()];
        assert!(allows_with_rules(
            "cat <<'EOF'\r\nharmless data\r\nEOF\r\nnpm run test:integration",
            &rules
        ));
        assert!(denies_with_rules(
            "cat <<'EOF'\r\nharmless data\r\nEOF\r\nplaywright run",
            &rules
        ));
    }

    #[test]
    fn generic_rule_unterminated_heredoc_does_not_swallow_trailing_command() {
        // A heredoc whose terminator never appears (malformed or
        // adversarial input, e.g. a deliberately mismatched delimiter)
        // must not cause every subsequent line to be silently dropped
        // from scanning — that would let a real trailing invocation
        // slip through unscanned. Fail toward scanning more, not less.
        let rules = [playwright_rule()];
        assert!(denies_with_rules(
            "cat <<'EOF'\nharmless data\nplaywright run",
            &rules
        ));
        assert!(denies_with_rules(
            "cat <<'EOF'\nharmless data\nNOT_THE_REAL_DELIMITER\nplaywright run",
            &rules
        ));
    }

    #[test]
    fn generic_rule_arithmetic_left_shift_is_not_a_heredoc() {
        // `$((n << 1))` is arithmetic left-shift, not heredoc
        // redirection. Regression test for a real bug found in review:
        // misidentifying it as a heredoc start would strip every
        // subsequent line (looking for a nonexistent terminator),
        // silently dropping a real trailing invocation from scanning.
        let rules = [playwright_rule()];
        assert!(denies_with_rules(
            "echo $((n << 1))\nplaywright run",
            &rules
        ));
    }

    #[test]
    fn generic_rule_quoted_double_angle_is_not_a_heredoc() {
        // `<<` appearing inside a quoted string (e.g. as literal text
        // being grepped for) is not a heredoc redirection either.
        let rules = [playwright_rule()];
        assert!(denies_with_rules(
            "grep \"a << EOF\" f\nplaywright run",
            &rules
        ));
    }

    #[test]
    fn generic_rule_denied_across_literal_newline_outside_heredoc() {
        let rules = [playwright_rule()];
        assert!(denies_with_rules("echo hi\nplaywright run", &rules));
    }

    #[test]
    fn generic_rule_allowed_with_passthrough_prefix() {
        let rules = [playwright_rule()];
        assert!(allows_with_rules("soldr playwright run", &rules));
    }

    #[test]
    fn generic_rule_passthrough_produces_helpful_log_message() {
        let rules = [playwright_rule()];
        let result = eval_with_rules("soldr playwright run", &rules);
        assert!(result.reason.is_none());
        let message = result
            .log_messages
            .iter()
            .find(|m| m.contains("BAD_CMD_PASSTHROUGH"))
            .expect("passthrough should log a helpful message");
        assert!(message.contains("no-raw-playwright"));
        assert!(message.contains("soldr"));
        assert!(message.contains("soldr playwright run"));
    }

    #[test]
    fn generic_rule_passthrough_prefix_is_a_quotable_glob() {
        // Use a fictional wrapper name (not "soldr", which is
        // universally trusted regardless of passthrough_prefixes — see
        // `generic_rule_passthrough_prefix_not_configured_still_denies`)
        // so this test isolates glob-quotability specifically.
        let mut rule = playwright_rule();
        rule.passthrough_prefixes = vec!["myproxy-*".to_string()];
        let rules = [rule];
        // Prefixes matching the glob are recognized wrappers -> the rule
        // is cleared and does not re-fire on what follows.
        assert!(allows_with_rules("myproxy-v2 playwright run", &rules));
        assert!(allows_with_rules("myproxy-nightly playwright run", &rules));
        // A wrapper word that does NOT match the glob (bare "myproxy",
        // no suffix) is just an unrecognized program; "playwright" is
        // its argument, not a nested invocation — same principle as
        // `rg playwright` staying allowed.
        assert!(allows_with_rules("myproxy playwright run", &rules));
        // The glob passthrough config must not weaken base matching:
        // a direct, unwrapped invocation is still denied.
        assert!(denies_with_rules("playwright run", &rules));
    }

    #[test]
    fn generic_rule_passthrough_prefix_regex_mode_applies_to_whole_set() {
        let mut rule = playwright_rule();
        rule.match_mode = MatchMode::Regex;
        rule.pattern = "playwright".to_string();
        rule.passthrough_prefixes = vec!["^soldr(-\\w+)?$".to_string()];
        let rules = [rule];
        assert!(allows_with_rules("soldr playwright run", &rules));
        assert!(allows_with_rules("soldr-nightly playwright run", &rules));
        // "soldrx" doesn't match the regex -> not a recognized wrapper,
        // so "playwright" is just its argument, not a nested invocation.
        assert!(allows_with_rules("soldrx playwright run", &rules));
        // Regex-mode passthrough config must not weaken base matching:
        // a direct, unwrapped invocation is still denied.
        assert!(denies_with_rules("playwright run", &rules));
    }

    #[test]
    fn generic_rule_passthrough_does_not_blanket_exempt_other_rules() {
        let mut foo_rule = playwright_rule();
        foo_rule.id = Some("no-foo".to_string());
        foo_rule.pattern = "foo".to_string();
        foo_rule.passthrough_prefixes = Vec::new();
        let rules = [playwright_rule(), foo_rule];
        assert!(allows_with_rules("soldr playwright run", &rules));
        assert!(denies_with_rules("soldr foo run", &rules));
    }

    #[test]
    fn generic_rule_passthrough_prefix_not_configured_still_denies() {
        // `soldr` must be treated as a universally-trusted transparent
        // wrapper for scan advancement purposes, independent of whether
        // *this particular* rule lists it in its own
        // `passthrough_prefixes` — otherwise a rule with no passthrough
        // config at all would incorrectly let `soldr <its bad program>`
        // through just because nothing ever advances the scan past
        // `soldr`. Regression test for a real bug found in review: this
        // must hold even when `foo_rule` is the *only* configured rule
        // (no other rule's passthrough incidentally causes advancement).
        let mut foo_rule = playwright_rule();
        foo_rule.id = Some("no-foo".to_string());
        foo_rule.pattern = "foo".to_string();
        foo_rule.passthrough_prefixes = Vec::new();
        let rules = [foo_rule];
        assert!(denies_with_rules("soldr foo run", &rules));
    }

    #[test]
    fn generic_rule_soldr_cargo_still_allowed_regression() {
        let rules = [playwright_rule()];
        assert!(allows_with_rules(
            &format!("soldr {TOOL_RS_BUILD} build"),
            &rules
        ));
    }

    #[test]
    fn generic_rule_exact_token_not_substring_or_prefix() {
        let mut rule = playwright_rule();
        rule.pattern = "play".to_string();
        let rules = [rule];
        assert!(allows_with_rules("playwright run", &rules));
        assert!(allows_with_rules("playlist-gen run", &rules));
        assert!(denies_with_rules("play run", &rules));
    }

    #[test]
    fn generic_rule_override_allowed_when_id_and_reason_match() {
        let rules = [playwright_rule_overridable()];
        temp_env(
            BAD_CMD_OVERRIDE_ENV,
            "no-raw-playwright:debugging flaky selector",
            || {
                let result = eval_with_rules("playwright run", &rules);
                assert!(result.reason.is_none());
                let message = result
                    .log_messages
                    .iter()
                    .find(|m| m.contains("BAD_CMD_OVERRIDE"))
                    .expect("override should log a helpful message");
                assert!(message.contains("no-raw-playwright"));
                assert!(message.contains("debugging flaky selector"));
            },
        );
    }

    #[test]
    fn generic_rule_override_hint_in_deny_message_helps_agent_construct_bypass() {
        // Serialize against other tests in this module that mutate
        // `CLUD_BAD_CMD_OVERRIDE` (process-global): without this, a
        // concurrently-running override test could make this rule's
        // "denied without an override set" assumption spuriously false.
        temp_env(BAD_CMD_OVERRIDE_ENV, "unrelated-rule:reason", || {
            let overridable = [playwright_rule_overridable()];
            let deny_message = eval_with_rules("playwright run", &overridable)
                .reason
                .expect("denied without an override set");
            assert!(deny_message.contains(BAD_CMD_OVERRIDE_ENV));
            assert!(deny_message.contains("no-raw-playwright"));
            assert!(deny_message.contains("environment variable"));

            let non_overridable = [playwright_rule()];
            let deny_message_no_hint = eval_with_rules("playwright run", &non_overridable)
                .reason
                .expect("denied without an override set");
            assert!(!deny_message_no_hint.contains(BAD_CMD_OVERRIDE_ENV));
        });
    }

    #[test]
    fn generic_rule_override_denied_when_id_mismatches() {
        let rules = [playwright_rule_overridable()];
        temp_env(BAD_CMD_OVERRIDE_ENV, "some-other-rule:reason", || {
            assert!(denies_with_rules("playwright run", &rules));
        });
    }

    #[test]
    fn generic_rule_override_denied_when_reason_missing() {
        let rules = [playwright_rule_overridable()];
        temp_env(BAD_CMD_OVERRIDE_ENV, "no-raw-playwright", || {
            assert!(denies_with_rules("playwright run", &rules));
        });
        temp_env(BAD_CMD_OVERRIDE_ENV, "no-raw-playwright:", || {
            assert!(denies_with_rules("playwright run", &rules));
        });
    }

    #[test]
    fn generic_rule_override_denied_when_rule_opts_out() {
        let rule = playwright_rule();
        assert!(!rule.allow_override, "default rule must not be overridable");
        let rules = [rule];
        temp_env(BAD_CMD_OVERRIDE_ENV, "no-raw-playwright:reason", || {
            assert!(denies_with_rules("playwright run", &rules));
        });
    }

    #[test]
    fn generic_rule_override_denied_for_ruleless_id() {
        let mut rule = playwright_rule_overridable();
        rule.id = None;
        let rules = [rule];
        temp_env(BAD_CMD_OVERRIDE_ENV, "anything:reason", || {
            assert!(denies_with_rules("playwright run", &rules));
        });
    }

    #[test]
    fn generic_rules_and_rust_tools_coexist_in_same_segment_scan() {
        let rules = [playwright_rule()];
        assert!(denies_with_rules(
            &format!("playwright run && {TOOL_RS_BUILD} build"),
            &rules
        ));
        assert!(denies_with_rules(
            &format!("{TOOL_RS_BUILD} build && playwright run"),
            &rules
        ));
    }

    #[test]
    fn generic_no_rules_configured_allows_all() {
        assert!(allows_with_rules("playwright run", &[]));
    }

    /// Serializes env-var mutation across tests in this module (env is
    /// process-global) and restores the prior value afterward.
    fn temp_env(key: &str, value: &str, f: impl FnOnce()) {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        f();
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
