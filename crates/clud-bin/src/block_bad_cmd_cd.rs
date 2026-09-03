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
//!
//! ## `"auto"` is a three-level resolver (#967 Phase 5, #966 D13)
//!
//! `"auto"` resolves against the hook environment at fire time:
//!
//! - any cwd-sensitive **raw** hook in scope (still in `.claude/settings*.json`
//!   or `.codex/hooks.json`, so the harness fires it unrooted) → **strict**:
//!   the session cwd must be a registered root;
//! - fully dispatcher-managed (`.clud/hooks.json` opt-in) or all raw hooks
//!   cwd-safe → **relaxed**: `cd` freely within the registered trees, block
//!   only escaping all of them;
//! - no hooks / not a repo → **off**.
//!
//! Migrating to `.clud/hooks.json` is what *earns* the relaxation — a built-in
//! adoption incentive (DD-063). [`drift_warning`] is the `CwdChanged` reactive
//! backstop for drift the PreToolUse scanner cannot see (DD-064).

use super::*;

/// Rule id an agent names in `CLUD_BAD_CMD_OVERRIDE` to bypass this guard
/// for a single call.
pub(super) const BLOCK_CD_RULE_ID: &str = "block-cd";

/// How strictly session-mutating `cd`s are policed, after `"auto"` has been
/// resolved against the environment (#966 §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CdPolicy {
    /// No cd policing at all.
    Off,
    /// The relaxed level of `"auto"`: `cd` freely within the registered repo
    /// trees; deny only a `cd` whose resolved target escapes **every**
    /// registered root. This is the mode a repo earns by migrating its hooks
    /// to `.clud/hooks.json`, where the dispatcher makes them cwd-immune
    /// (D13, DD-063).
    Relaxed,
    /// The session cwd must *be* a registered root: only `cd <root>` is
    /// allowed, subdirectories included in the denial. This is the mode for
    /// repos with cwd-sensitive raw hooks (still fired by the harness,
    /// unrooted), which would break on any drift.
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

/// The concrete directories a command's session-level `cd`s would move to.
///
/// Containment needs these because a `cd` relocates where the rest of the
/// command acts: `cd .extern-repos/dep && make` touches the sub-repo even
/// though the payload cwd still says otherwise (#967 Phase 3).
pub(super) fn session_cd_targets(
    command_text: &str,
    dialect: ShellDialect,
    cwd: &Path,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    scan_session_cd(command_text, dialect, cwd, home)
        .into_iter()
        .filter_map(|occurrence| match occurrence.target {
            CdTarget::Path(path) => Some(path),
            CdTarget::Unresolvable | CdTarget::NoOp => None,
        })
        .collect()
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
        // Relaxed cannot prove an unresolvable target leaves the tree, and
        // this layer is hygiene: it narrows only on evidence.
        (CdTarget::Unresolvable, CdPolicy::Relaxed) => None,
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
        (CdTarget::Path(path), CdPolicy::Relaxed) => {
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
        "Blocked `cd {described}`: it moves the session cwd outside the registered repos \
         ({roots}) for every later tool call, where repo-relative tooling stops resolving and \
         the session loses its orientation. Use `(cd DIR && CMD)`, `git -C DIR ...`, or an \
         absolute path instead; a subshell `cd` is always allowed because it cannot leak. To \
         change the policy set `bash.block_cd` in .clud/settings.json, or bypass this one call \
         with CLUD_BAD_CMD_OVERRIDE={rule}:<reason>.",
        roots = roots_display(roots),
        rule = BLOCK_CD_RULE_ID,
    )
}

// ---------------------------------------------------------------------
// `"auto"` resolution: how cwd-sensitive are the hooks in scope?
// ---------------------------------------------------------------------

/// What a scan of the hook environment found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookCwdScan {
    /// Any hook command at all is configured for this repo or user in the
    /// **raw** frontend configs (`.claude/settings*.json`, `.codex/hooks.json`).
    pub any_hooks: bool,
    /// Whether the repo has opted into `.clud/hooks.json` (#967 Phase 2). Its
    /// declared hooks are dispatcher-managed and therefore cwd-immune (cwd +
    /// `CLUD_PROJECT_DIR` = the declaring repo's root, D10), so their command
    /// text never counts toward sensitivity — this flag is what lets `"auto"`
    /// resolve to relaxed for a migrated repo whose raw frontend hooks are
    /// gone or cwd-safe (D13, DD-063).
    pub dispatcher_managed: bool,
    /// Hook commands that resolve a relative path against the inherited
    /// cwd, with the file they came from. First entry is quoted in denials.
    pub sensitive: Vec<SensitiveHook>,
    /// Hook commands carrying the broken `git rev-parse` self-rooting
    /// prefix, which `hook_health` reports with the one-line fix at launch and
    /// under `clud --fix-hooks`. A subset of `sensitive`, tracked separately
    /// because it has a specific remedy.
    pub broken_git_prefix: Vec<SensitiveHook>,
    /// Hook commands that resolve their project root by walking **up from
    /// `$PWD`** looking for a marker file (#972). Also a subset of
    /// `sensitive`, and also tracked separately for its own remedy.
    pub pwd_walk_prefix: Vec<SensitiveHook>,
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
///
/// The scan covers only the **raw** frontend configs. A `.clud/hooks.json`
/// opt-in is recorded as [`HookCwdScan::dispatcher_managed`] instead — those
/// hooks are rooted by the dispatcher no matter what their command text looks
/// like, so they never make a repo strict (#966 D10, D13).
pub fn scan_hook_cwd_sensitivity(repo_root: &Path, home: Option<&Path>) -> HookCwdScan {
    let mut scan = HookCwdScan {
        dispatcher_managed: crate::clud_hooks::discover(repo_root).is_some(),
        ..HookCwdScan::default()
    };
    for hook in frontend_hook_commands(repo_root, home) {
        scan.any_hooks = true;
        if has_broken_git_rev_parse_prefix(&hook.command) {
            scan.broken_git_prefix.push(hook.clone());
        }
        if has_pwd_walk_root_prefix(&hook.command) {
            scan.pwd_walk_prefix.push(hook.clone());
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

/// Whether the command resolves its project root by walking **up from `$PWD`**
/// until it finds a marker file, rather than using the session's project root
/// (#972).
///
/// The shape is a loop over `dirname` testing for `pyproject.toml` (or similar)
/// and then `cd`-ing to whatever it lands on. Like the broken `git rev-parse`
/// prefix, it *looks* self-rooting — there is a real `cd` to a real absolute
/// path — which is why it is worth naming separately from the generic
/// relative-path warning. The hazard is not that the path is unrooted; it is
/// that the root is chosen by the shell's cwd.
///
/// That matters because `clud-extern-repos` puts a **complete sibling project**
/// at `<parent>/.extern-repos/<name>/`, with its own `pyproject.toml` and its
/// own hook scripts. A hook fired while the shell sits in that checkout walks
/// up, finds the dependent project first, and runs *its* hook. Cross-repo work
/// is exactly when the shell lives there, so this misfires precisely in the
/// workflow the convention exists to support — and when the dependent project
/// is Rust-backed, `uv run` there is a native build, not a script launch.
pub fn has_pwd_walk_root_prefix(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    // A `$PWD` seed (or a bare `cd`-less walk) plus a `dirname` climb is the
    // signature. Requiring both keeps a command that merely mentions `$PWD`,
    // or one that calls `dirname` once on a known path, out of it.
    let seeds_from_pwd = lower.contains("$pwd") || lower.contains("${pwd}");
    let climbs = lower.contains("dirname");
    let tests_a_marker = lower.contains("pyproject.toml")
        || lower.contains("package.json")
        || lower.contains("cargo.toml")
        || lower.contains(".git");
    seeds_from_pwd && climbs && tests_a_marker
}

/// The replacement clud recommends for the `$PWD`-walk prefix.
pub const PWD_WALK_PREFIX_FIX: &str =
    "cd \"$CLAUDE_PROJECT_DIR\" && ... (the repo the hook is installed in, which is what it \
     should act on; a `$PWD` walk finds whichever project the shell happens to be standing in, \
     including a sibling checkout under .extern-repos/)";

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

/// Resolve `"auto"` against the environment, per #966 §8 — the three-level
/// resolver of #967 Phase 5:
///
/// | environment | `"auto"` resolves to |
/// | --- | --- |
/// | any cwd-sensitive raw hooks in scope (unmigrated repo, relative-path commands in `.claude/settings.json`) | **strict** — pin cwd to the registered roots |
/// | fully on clud hooks (dispatcher-managed; hooks cwd-immune), or raw hooks all cwd-safe | **relaxed** — `cd` freely within registered repo trees; block only escaping all of them |
/// | no hooks / not a repo | **off** |
///
/// Migrating to `.clud/hooks.json` is what *earns* the relaxation (D13): a
/// migrated repo's hooks are rooted by the dispatcher, so drift breaks
/// nothing and the policy loosens from "cwd must be a root" to "cwd must stay
/// inside the registered trees". Any cwd-sensitive **raw** hook still in
/// scope — the harness fires it unrooted, so drift would break it — keeps the
/// repo strict even if it has also migrated (DD-063).
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
            if !in_repo || (!scan.any_hooks && !scan.dispatcher_managed) {
                CdPolicy::Off
            } else if scan.sensitive.is_empty() {
                CdPolicy::Relaxed
            } else {
                CdPolicy::Strict
            }
        }
    }
}

/// Whether the session cwd landing at `new_cwd` violates the pinning policy —
/// the `CwdChanged` backstop's predicate (zackees/clud#967 Phase 5).
///
/// The PreToolUse scanner only sees `cd`s written in a tool call; an alias or
/// a script that chdirs moves the session cwd invisibly. This is the reactive
/// check for that drift. Hygiene only — nothing may depend on it for
/// correctness, because the upstream cwd contract is unstable (D12, DD-064).
///
/// Returns `None` when the policy is off, there are no roots, or the new cwd
/// still satisfies the policy: strict requires it to *be* a registered root,
/// relaxed only that it stay inside one of the registered trees.
pub(super) fn drift_warning(new_cwd: &Path, policy: CdPolicy, roots: &[PathBuf]) -> Option<String> {
    if policy == CdPolicy::Off || roots.is_empty() {
        return None;
    }
    let drifted = match policy {
        CdPolicy::Strict => !is_registered_root(new_cwd, roots),
        CdPolicy::Relaxed => !roots.iter().any(|root| is_within(new_cwd, root)),
        CdPolicy::Off => return None,
    };
    if !drifted {
        return None;
    }
    let inside = match policy {
        CdPolicy::Strict => "which the `bash.block_cd` policy pins to a registered root",
        CdPolicy::Relaxed => {
            "outside the registered repos the `bash.block_cd` policy pins the \
                              session to"
        }
        CdPolicy::Off => unreachable!(),
    };
    Some(format!(
        "[clud] CwdChanged: the session cwd moved to {}, {inside} ({roots}). A chdir from an \
         alias or a script can bypass the PreToolUse scanner, which is why this event exists. \
         Nothing was blocked and clud's hooks stay correctly rooted (containment is resolved per \
         path), but `cd` back into a registered repo to restore the invariant. This is a \
         hygiene warning; to silence it set `bash.block_cd` to false or migrate every hook to \
         .clud/hooks.json.",
        crate::path_norm::display_slash(new_cwd),
        roots = roots_display(roots),
    ))
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
