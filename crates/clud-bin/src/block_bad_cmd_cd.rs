//! `bash.block_cd` — session cwd pinning (zackees/clud#966 §8, #967 Phase 1).
//!
//! A `cd` in a Bash tool call mutates the **session** cwd, not just that one
//! command's: every later tool call inherits the moved cwd. Anything that
//! resolves a relative path against it breaks at once — most damagingly
//! project hooks, which are conventionally written as repo-relative script
//! paths (`uv run python ci/hooks/check-on-stop.py`). One stray `cd` can
//! therefore wedge a session so thoroughly that no tool can run.
//!
//! This module is the preventive layer: it scans a command for `cd`s that
//! would move the *session* cwd and denies the ones the active policy
//! forbids. Two properties keep it honest:
//!
//! - **Only session-mutating `cd`s count.** A `cd` inside a `(...)` subshell,
//!   a `$(...)` substitution, or a nested shell (`bash -c '...'`) runs in a
//!   child process and cannot leak, so it is always allowed.
//! - **Pinning is hygiene, not correctness.** The upstream cwd contract is
//!   unstable (anthropics/claude-code#83636 resets it, #76708 fails to
//!   persist it, #84685 shares it across concurrent subagents), so nothing
//!   may depend on this for correctness. It protects the agent's own
//!   relative-path commands and keeps the invariant the harness snaps back
//!   to anyway. Rooting hooks properly is the dispatcher's job (#967
//!   Phase 2+).
//!
//! ## Why this is not a `bad_commands` rule
//!
//! The DD-016 engine matches an executable plus argument-token predicates.
//! Deciding whether a `cd` target escapes the registered roots requires
//! *resolving* the argument against those roots — path resolution, not
//! pattern matching. A regex is either too blunt (denies `cd src/`) or
//! misses `cd ../..`, `cd $HOME`, `cd %USERPROFILE%`, and absolute paths.
//! See DD-047.

use super::*;

/// Rule id an agent names in `CLUD_BAD_CMD_OVERRIDE` to bypass this guard
/// for a single call.
pub(super) const BLOCK_CD_RULE_ID: &str = "block-cd";

/// How strictly session-mutating `cd`s are policed, after `"auto"` has been
/// resolved against the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CdPolicy {
    /// No cd policing at all.
    Off,
    /// Deny only `cd`s whose resolved target escapes every registered root's
    /// tree. `cd src/` stays allowed.
    EscapeOnly,
    /// The session cwd must *be* a registered root: only `cd <root>` is
    /// allowed, subdirectories included in the denial. This is the mode for
    /// repos whose hooks would break on any drift.
    Strict,
}

/// One session-mutating `cd` found in a command, with its target already
/// classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CdOccurrence {
    /// The `cd` argument exactly as written, for the denial message.
    /// `None` for a bare `cd`.
    pub(super) raw_target: Option<String>,
    pub(super) target: CdTarget,
}

/// What a `cd` argument resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CdTarget {
    /// Resolved to a concrete absolute path.
    Path(PathBuf),
    /// A variable, command substitution, or `cd -`: the target cannot be
    /// known without running the shell.
    Unresolvable,
    /// The `cd` provably does not move anywhere (PowerShell/cmd bare `cd`).
    NoOp,
}

// ---------------------------------------------------------------------
// Cheap pre-filter (hot path).
// ---------------------------------------------------------------------

/// Conservative word-boundary check for whether `command_text` could contain
/// a directory-changing builtin, used to skip policy resolution — which
/// reads settings and hook config files — for the overwhelming majority of
/// tool calls that contain no `cd` at all.
///
/// Deliberately loose: a false positive costs one settings read, while a
/// false negative would silently disable the guard. Word-boundary rather
/// than substring so `curl -sL` does not read as `sl`, but still only a
/// pre-filter — command *position* is decided by the scanner.
pub(super) fn command_may_change_directory(command_text: &str, dialect: ShellDialect) -> bool {
    let lower = command_text.to_ascii_lowercase();
    cd_builtins(dialect)
        .iter()
        .any(|name| contains_word(&lower, name))
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let mut from = 0usize;
    while let Some(found) = haystack[from..].find(needle) {
        let start = from + found;
        let end = start + needle.len();
        let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let after_ok = end == bytes.len() || !is_word_byte(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
        if from >= haystack.len() {
            break;
        }
    }
    false
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.' || byte == b'-'
}

/// The directory-changing builtins that mutate the *session* cwd, per shell.
///
/// `pushd`/`popd` are deliberately out of scope: they are rare in agent
/// commands and normally balanced within one call.
fn cd_builtins(dialect: ShellDialect) -> &'static [&'static str] {
    match dialect {
        ShellDialect::Posix => &["cd"],
        ShellDialect::PowerShell => &["cd", "chdir", "sl", "set-location"],
        ShellDialect::Cmd => &["cd", "chdir"],
    }
}

// ---------------------------------------------------------------------
// Scanner: session-mutating `cd`s only.
// ---------------------------------------------------------------------

/// Find every `cd` in `command_text` that would move the session cwd.
///
/// Skips subshells, command substitutions, and nested shells, all of which
/// change only a child process's directory. `cwd` is the base that relative
/// targets resolve against; `home` supplies `~` / `$HOME` expansion.
pub(super) fn scan_session_cd(
    command_text: &str,
    dialect: ShellDialect,
    cwd: &Path,
    home: Option<&Path>,
) -> Vec<CdOccurrence> {
    let stripped = strip_heredoc_bodies(command_text);
    let mut found = Vec::new();
    for segment in top_level_segments(&stripped, dialect) {
        let words = command_words(&segment);
        if words.is_empty() {
            continue;
        }
        // `bash -c 'cd /tmp'` changes only the child's directory.
        if nested_shell_command(&words, dialect).is_some() {
            continue;
        }
        let program = program_name(&words[0]).to_ascii_lowercase();
        if !cd_builtins(dialect).contains(&program.as_str()) {
            continue;
        }
        let raw_target = cd_argument(&words[1..], dialect);
        let target = match raw_target.as_deref() {
            None => bare_cd_target(dialect, home),
            Some(argument) => resolve_cd_argument(argument, cwd, home),
        };
        found.push(CdOccurrence { raw_target, target });
    }
    found
}

/// Split a command into the segments that run in the session's own shell.
///
/// Everything inside `(...)` — which covers `$(...)` substitutions too,
/// since the paren depth rises either way — and inside backticks is dropped
/// rather than returned, because a `cd` there cannot escape its child.
fn top_level_segments(command_text: &str, dialect: ShellDialect) -> Vec<String> {
    let chars: Vec<char> = command_text.chars().collect();
    let mut segments = Vec::new();
    let mut buf = String::new();
    let mut quote: Option<char> = None;
    let mut depth = 0usize;
    let mut i = 0usize;

    let push = |buf: &mut String, segments: &mut Vec<String>| {
        let segment = buf.trim();
        if !segment.is_empty() {
            segments.push(segment.to_string());
        }
        buf.clear();
    };

    while i < chars.len() {
        let ch = chars[i];

        if let Some(q) = quote {
            if depth == 0 {
                buf.push(ch);
            }
            if q != '\'' && is_shell_escape(ch, dialect) && i + 1 < chars.len() {
                if depth == 0 {
                    buf.push(chars[i + 1]);
                }
                i += 2;
                continue;
            }
            if ch == q {
                quote = None;
            }
            i += 1;
            continue;
        }

        if ch == '\'' || ch == '"' || ch == '`' {
            quote = Some(ch);
            if depth == 0 {
                buf.push(ch);
            }
            i += 1;
            continue;
        }

        if is_shell_escape(ch, dialect) && i + 1 < chars.len() {
            if depth == 0 {
                buf.push(ch);
                buf.push(chars[i + 1]);
            }
            i += 2;
            continue;
        }

        if ch == '#' && dialect != ShellDialect::Cmd && is_shell_comment_start(&chars, i) {
            while i < chars.len() && !matches!(chars[i], '\r' | '\n') {
                i += 1;
            }
            continue;
        }

        if ch == '(' {
            // A subshell or substitution starts here; the session cwd is
            // safe from anything inside it, so end the current segment and
            // swallow the group.
            if depth == 0 {
                push(&mut buf, &mut segments);
            }
            depth += 1;
            i += 1;
            continue;
        }
        if ch == ')' {
            depth = depth.saturating_sub(1);
            i += 1;
            continue;
        }
        if depth > 0 {
            i += 1;
            continue;
        }

        let is_separator = matches!(ch, ';' | '\n' | '\r' | '|' | '&');
        if is_separator {
            push(&mut buf, &mut segments);
            // Consume a paired `&&` / `||` in one step.
            if (ch == '&' || ch == '|') && i + 1 < chars.len() && chars[i + 1] == ch {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }

        buf.push(ch);
        i += 1;
    }
    push(&mut buf, &mut segments);
    segments
}

/// Pick the directory operand out of a `cd`'s arguments, skipping flags.
fn cd_argument(args: &[String], dialect: ShellDialect) -> Option<String> {
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        // `cd -` is an operand, not a flag: it means "the previous directory".
        if arg == "-" {
            return Some(arg.clone());
        }
        let lower = arg.to_ascii_lowercase();
        if dialect == ShellDialect::PowerShell
            && matches!(lower.as_str(), "-path" | "-literalpath")
            && index + 1 < args.len()
        {
            return Some(args[index + 1].clone());
        }
        let is_flag = match dialect {
            // `/d` is a cmd flag; on POSIX a leading `/` is an absolute path.
            ShellDialect::Cmd => arg.starts_with('-') || arg.starts_with('/'),
            _ => arg.starts_with('-'),
        };
        if is_flag {
            index += 1;
            continue;
        }
        return Some(arg.clone());
    }
    None
}

/// Where a bare `cd` (no operand) lands, which differs by shell.
///
/// POSIX `cd` goes to `$HOME`. PowerShell's `Set-Location` with no operand
/// and cmd's `cd` do not move at all — cmd merely prints the current
/// directory. Verified on Windows PowerShell 5.1.
fn bare_cd_target(dialect: ShellDialect, home: Option<&Path>) -> CdTarget {
    match dialect {
        ShellDialect::Posix => match home {
            Some(home) => CdTarget::Path(lexically_normalize(home)),
            None => CdTarget::Unresolvable,
        },
        ShellDialect::PowerShell | ShellDialect::Cmd => CdTarget::NoOp,
    }
}

/// Resolve a `cd` operand to an absolute path where that is possible
/// without running a shell.
///
/// A small set of home-directory spellings is expanded because they are the
/// common way an agent leaves a repo (`cd ~`, `cd $HOME`, `cd
/// %USERPROFILE%`). Anything still carrying a `$`, `%`, or substitution is
/// reported unresolvable rather than guessed at.
fn resolve_cd_argument(argument: &str, cwd: &Path, home: Option<&Path>) -> CdTarget {
    let trimmed = unquote(argument.trim());
    if trimmed.is_empty() {
        // `cd ""` is a no-op in POSIX shells.
        return CdTarget::NoOp;
    }
    if trimmed == "-" {
        return CdTarget::Unresolvable;
    }
    if let Some(expanded) = expand_home_prefix(&trimmed, home) {
        return CdTarget::Path(resolve_against(cwd, &expanded));
    }
    if trimmed.contains('$') || trimmed.contains('%') || trimmed.contains('`') {
        return CdTarget::Unresolvable;
    }
    CdTarget::Path(resolve_against(cwd, &trimmed))
}

/// Expand the handful of home-directory spellings this guard recognizes,
/// returning `None` when `raw` does not start with one.
fn expand_home_prefix(raw: &str, home: Option<&Path>) -> Option<String> {
    const HOME_TOKENS: &[&str] = &[
        "~",
        "$HOME",
        "${HOME}",
        "$env:USERPROFILE",
        "$env:HOME",
        "%USERPROFILE%",
        "%HOME%",
    ];
    let home = home?;
    for token in HOME_TOKENS {
        let matches_token = raw.eq_ignore_ascii_case(token);
        let with_separator = raw.len() > token.len()
            && raw[..token.len()].eq_ignore_ascii_case(token)
            && matches!(raw.as_bytes()[token.len()], b'/' | b'\\');
        if matches_token {
            return Some(home.to_string_lossy().into_owned());
        }
        if with_separator {
            let rest = &raw[token.len() + 1..];
            let mut joined = home.to_string_lossy().into_owned();
            joined.push(std::path::MAIN_SEPARATOR);
            joined.push_str(rest);
            return Some(joined);
        }
    }
    None
}

fn unquote(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' || first == b'\'') && first == last {
            return raw[1..raw.len() - 1].to_string();
        }
    }
    raw.to_string()
}

// ---------------------------------------------------------------------
// Decision.
// ---------------------------------------------------------------------

/// Decide whether any `cd` in `command_text` must be denied.
///
/// `roots` is the registered-root set the session cwd is pinned to. In
/// Phase 1 that is the containing repo root; the typed registry of #967
/// Phase 3 widens it without changing this signature.
pub(super) fn cd_denial_reason(
    command_text: &str,
    dialect: ShellDialect,
    policy: CdPolicy,
    cwd: &Path,
    roots: &[PathBuf],
    home: Option<&Path>,
    sensitive_hint: Option<&str>,
) -> Option<String> {
    if policy == CdPolicy::Off || roots.is_empty() {
        return None;
    }
    for occurrence in scan_session_cd(command_text, dialect, cwd, home) {
        if let Some(reason) = occurrence_denial(&occurrence, policy, roots, sensitive_hint) {
            return Some(reason);
        }
    }
    None
}

fn occurrence_denial(
    occurrence: &CdOccurrence,
    policy: CdPolicy,
    roots: &[PathBuf],
    sensitive_hint: Option<&str>,
) -> Option<String> {
    let described = occurrence
        .raw_target
        .clone()
        .unwrap_or_else(|| "<home>".to_string());
    match (&occurrence.target, policy) {
        (CdTarget::NoOp, _) => None,
        // Escape-only cannot prove an unresolvable target leaves the tree, and
        // this layer is hygiene: it narrows only on evidence.
        (CdTarget::Unresolvable, CdPolicy::EscapeOnly) => None,
        (CdTarget::Unresolvable, CdPolicy::Strict) => Some(strict_message(
            &described,
            roots,
            sensitive_hint,
            "its target cannot be resolved without running the shell",
        )),
        (CdTarget::Path(path), CdPolicy::Strict) => {
            if is_registered_root(path, roots) {
                None
            } else {
                Some(strict_message(
                    &described,
                    roots,
                    sensitive_hint,
                    "it is not a registered repo root",
                ))
            }
        }
        (CdTarget::Path(path), CdPolicy::EscapeOnly) => {
            if roots.iter().any(|root| is_within(path, root)) {
                None
            } else {
                Some(escape_message(&described, roots))
            }
        }
        (_, CdPolicy::Off) => None,
    }
}

fn is_registered_root(path: &Path, roots: &[PathBuf]) -> bool {
    let key = crate::path_norm::normalize_for_key(path);
    roots
        .iter()
        .any(|root| crate::path_norm::normalize_for_key(root) == key)
}

/// Whether `path` is `root` or lives underneath it, compared on normalized
/// keys so Windows drive-letter and separator casing do not decide it.
fn is_within(path: &Path, root: &Path) -> bool {
    let path_key = crate::path_norm::normalize_for_key(path);
    let root_key = crate::path_norm::normalize_for_key(root);
    if path_key == root_key {
        return true;
    }
    let prefix = if root_key.ends_with('/') {
        root_key
    } else {
        format!("{root_key}/")
    };
    path_key.starts_with(&prefix)
}

fn roots_display(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|root| crate::path_norm::display_slash(root))
        .collect::<Vec<_>>()
        .join(", ")
}

fn strict_message(
    described: &str,
    roots: &[PathBuf],
    sensitive_hint: Option<&str>,
    why: &str,
) -> String {
    let because = match sensitive_hint {
        Some(hint) => format!(
            " This repo has a cwd-sensitive hook ({hint}), which resolves its script path against \
             whatever cwd it inherits, so any drift breaks it."
        ),
        None => String::new(),
    };
    format!(
        "Blocked `cd {described}`: it mutates the session cwd for every later tool call, and {why}.\
         {because} Run the command without moving the session instead — `(cd DIR && CMD)`, \
         `git -C DIR ...`, `cargo --manifest-path DIR/Cargo.toml ...`, or an absolute path; a \
         subshell `cd` is always allowed because it cannot leak. `cd` back to a registered root \
         ({roots}) stays allowed as the way to recover. To change the policy set \
         `bash.block_cd` in .clud/settings.json, or bypass this one call with \
         CLUD_BAD_CMD_OVERRIDE={rule}:<reason>.",
        roots = roots_display(roots),
        rule = BLOCK_CD_RULE_ID,
    )
}

fn escape_message(described: &str, roots: &[PathBuf]) -> String {
    format!(
        "Blocked `cd {described}`: it moves the session cwd outside the repo ({roots}) for every \
         later tool call, which breaks repo-relative hooks and tooling. Use `(cd DIR && CMD)`, \
         `git -C DIR ...`, or an absolute path instead; a subshell `cd` is always allowed because \
         it cannot leak. To change the policy set `bash.block_cd` in .clud/settings.json, or \
         bypass this one call with CLUD_BAD_CMD_OVERRIDE={rule}:<reason>.",
        roots = roots_display(roots),
        rule = BLOCK_CD_RULE_ID,
    )
}

// ---------------------------------------------------------------------
// `"auto"` resolution: how cwd-sensitive are the hooks in scope?
// ---------------------------------------------------------------------

/// What a scan of the frontends' hook configs found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookCwdScan {
    /// Any hook command at all is configured for this repo or user.
    pub any_hooks: bool,
    /// Hook commands that resolve a relative path against the inherited
    /// cwd, with the file they came from. First entry is quoted in denials.
    pub sensitive: Vec<SensitiveHook>,
    /// Hook commands carrying the broken `git rev-parse` self-rooting
    /// prefix, which `hook_health` reports with the one-line fix at launch and
    /// under `clud --fix-hooks`. A subset of `sensitive`, tracked separately
    /// because it has a specific remedy.
    pub broken_git_prefix: Vec<SensitiveHook>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SensitiveHook {
    pub source: PathBuf,
    pub command: String,
}

impl HookCwdScan {
    pub fn hint(&self) -> Option<String> {
        self.sensitive.first().map(|hook| {
            format!(
                "{} in {}",
                truncate_command(&hook.command),
                crate::path_norm::display_slash(&hook.source)
            )
        })
    }
}

fn truncate_command(command: &str) -> String {
    const LIMIT: usize = 60;
    let single_line = command.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= LIMIT {
        return format!("`{single_line}`");
    }
    let clipped: String = single_line.chars().take(LIMIT).collect();
    format!("`{clipped}…`")
}

/// Read every hook command configured for `repo_root` (and `home`), across
/// **all** events, and classify each for cwd sensitivity.
///
/// Deliberately not routed through `hook_health::inspect`, which parses only
/// `PreToolUse`: the wedge that motivated this work came from a `Stop` hook,
/// and this path also has to stay cheap enough for a per-tool-call hook.
pub fn scan_hook_cwd_sensitivity(repo_root: &Path, home: Option<&Path>) -> HookCwdScan {
    let mut scan = HookCwdScan::default();
    for hook in frontend_hook_commands(repo_root, home) {
        scan.any_hooks = true;
        if has_broken_git_rev_parse_prefix(&hook.command) {
            scan.broken_git_prefix.push(hook.clone());
        }
        if is_cwd_sensitive_hook_command(&hook.command) {
            scan.sensitive.push(hook);
        }
    }
    scan
}

/// Every hook command configured for `repo_root` (and `home`), across all
/// events and both frontends, paired with the file it came from.
///
/// Split out from the sensitivity scan because `hook_health` also needs the
/// raw list, to notice a command declared in *both* `.clud/hooks.json` and a
/// frontend's own settings — which would run twice, and only once rooted.
pub fn frontend_hook_commands(repo_root: &Path, home: Option<&Path>) -> Vec<SensitiveHook> {
    let mut found = Vec::new();
    let mut candidates = vec![
        repo_root.join(".claude").join("settings.json"),
        repo_root.join(".claude").join("settings.local.json"),
        repo_root.join(".codex").join("hooks.json"),
    ];
    if let Some(home) = home {
        candidates.push(home.join(".claude").join("settings.json"));
        candidates.push(home.join(".codex").join("hooks.json"));
    }
    for path in candidates {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        for command in hook_commands(&json) {
            found.push(SensitiveHook {
                source: path.clone(),
                command,
            });
        }
    }
    found
}

/// Every hook command string in a frontend config, regardless of event.
///
/// Both frontends nest as `hooks.<Event>[].hooks[].command`; codex also
/// accepts a root-level `<Event>` shape. Walking the values rather than
/// naming events keeps new upstream events covered for free.
fn hook_commands(json: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut roots = Vec::new();
    if let Some(hooks) = json.get("hooks") {
        roots.push(hooks);
    }
    roots.push(json);
    for root in roots {
        let Some(events) = root.as_object() else {
            continue;
        };
        for (_event, groups) in events {
            let Some(groups) = groups.as_array() else {
                continue;
            };
            for group in groups {
                let Some(handlers) = group.get("hooks").and_then(Value::as_array) else {
                    continue;
                };
                for handler in handlers {
                    if let Some(command) = handler.get("command").and_then(Value::as_str) {
                        let command = command.trim();
                        if !command.is_empty() && !out.iter().any(|seen| seen == command) {
                            out.push(command.to_string());
                        }
                    }
                }
            }
        }
    }
    out
}

/// Whether a hook command would resolve a path against the cwd it inherits.
///
/// A PATH binary (`clud-cmd-scan`) or an absolute path is immune. A relative
/// script path is not — and neither is the `git rev-parse` self-rooting
/// prefix, which is broken (see [`has_broken_git_rev_parse_prefix`]) and so
/// leaves the command exactly as exposed as if it had no prefix at all.
pub fn is_cwd_sensitive_hook_command(command: &str) -> bool {
    if has_broken_git_rev_parse_prefix(command) {
        return true;
    }
    if has_self_rooting_prefix(command) {
        return false;
    }
    command.split_whitespace().any(looks_like_relative_path)
}

/// Whether the command starts by planting itself at a session-constant
/// absolute root, which makes any relative path after it safe.
fn has_self_rooting_prefix(command: &str) -> bool {
    let head = command
        .split("&&")
        .next()
        .unwrap_or_default()
        .trim()
        .trim_start_matches("cd ")
        .trim();
    let head = unquote(head);
    if head.contains("CLAUDE_PROJECT_DIR") || head.contains("CLUD_PROJECT_DIR") {
        return true;
    }
    !head.is_empty() && is_absolute_pathish(&head)
}

/// Whether `raw` reads as an absolute path in *either* platform's spelling.
///
/// `Path::is_absolute` answers for the host only, and hook configs travel: a
/// `/usr/local/bin/guard.sh` command read on Windows would otherwise be
/// classified as a relative path and wrongly reported cwd-sensitive.
fn is_absolute_pathish(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    if matches!(bytes.first(), Some(b'/') | Some(b'\\')) {
        return true;
    }
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return true;
    }
    Path::new(raw).is_absolute()
}

/// The community `git rev-parse` self-rooting prefix, and why it is broken.
///
/// ```text
/// cd "$(git rev-parse --show-superproject-working-tree 2>/dev/null \
///     || git rev-parse --show-toplevel 2>/dev/null || echo .)" && ...
/// ```
///
/// `--show-superproject-working-tree` exits **0 with empty stdout** outside a
/// submodule, and `||` advances on a non-zero exit rather than on empty
/// output — so the `--show-toplevel` fallback is dead code, `cd ""` is a
/// silent no-op, and the hook resolves its script path against whatever cwd
/// it inherited. It only ever appeared to work because cwd happened to be
/// the repo root. `cd "$CLAUDE_PROJECT_DIR"` is the fix.
pub fn has_broken_git_rev_parse_prefix(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    lower.contains("--show-superproject-working-tree")
        && lower.contains("||")
        && lower.contains("rev-parse")
}

/// The replacement clud recommends for the broken prefix.
pub const GIT_REV_PARSE_PREFIX_FIX: &str =
    "cd \"$CLAUDE_PROJECT_DIR\" && ... (the project root where the session started; constant for \
     the whole session, no git invocation, no submodule edge case)";

fn looks_like_relative_path(token: &str) -> bool {
    if token.starts_with('-') || token.contains("://") {
        return false;
    }
    let unquoted = unquote(token);
    if unquoted.is_empty() {
        return false;
    }
    if unquoted.starts_with('$') || unquoted.starts_with('%') || unquoted.starts_with('~') {
        return false;
    }
    if !unquoted.contains('/') && !unquoted.contains('\\') {
        return false;
    }
    !is_absolute_pathish(&unquoted)
}

/// Resolve `"auto"` against the environment, per #966 §8.
///
/// Phase 1 has no dispatcher, so the relaxed level cannot be earned yet: a
/// repo with cwd-sensitive hooks gets strict pinning, one whose hooks are
/// all cwd-immune gets escape-only, and a repo with no hooks — or no repo at
/// all — gets nothing.
pub(super) fn resolve_policy(
    setting: crate::repo_clud_config::BlockCd,
    in_repo: bool,
    scan: &HookCwdScan,
) -> CdPolicy {
    use crate::repo_clud_config::BlockCd;
    match setting {
        BlockCd::Never => CdPolicy::Off,
        BlockCd::Always => {
            if in_repo {
                CdPolicy::Strict
            } else {
                CdPolicy::Off
            }
        }
        BlockCd::Auto => {
            if !in_repo || !scan.any_hooks {
                CdPolicy::Off
            } else if scan.sensitive.is_empty() {
                CdPolicy::EscapeOnly
            } else {
                CdPolicy::Strict
            }
        }
    }
}

/// Nearest ancestor of `start` (inclusive) containing a `.git` entry.
///
/// A lexical walk, deliberately not `loop_spec::git_root_from` — that
/// returns `start` itself when there is no repo, which would make "not in a
/// repo" indistinguishable from "repo at cwd" and silently arm the guard
/// outside repos. A `.git` *file* counts, so linked worktrees resolve.
pub(super) fn nearest_repo_root(start: &Path) -> Option<PathBuf> {
    let mut current = lexically_normalize(start);
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

#[cfg(test)]
#[path = "block_bad_cmd_cd_tests.rs"]
mod block_bad_cmd_cd_tests;
