//! Decision table for `bash.block_cd` (zackees/clud#967 Phase 1).
//!
//! The table is asserted directly — policy × dialect × target shape — rather
//! than through the hook binary, so a change of verdict shows up as a failing
//! row naming the case rather than as a hook exit code.

use super::*;
use crate::repo_clud_config::BlockCd;
use std::fs;
use tempfile::TempDir;

fn root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\repo")
    } else {
        PathBuf::from("/repo")
    }
}

fn home() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\Users\dev")
    } else {
        PathBuf::from("/home/dev")
    }
}

fn deny(command: &str, policy: CdPolicy) -> Option<String> {
    deny_in(command, policy, ShellDialect::Posix, &root())
}

fn deny_in(command: &str, policy: CdPolicy, dialect: ShellDialect, cwd: &Path) -> Option<String> {
    cd_denial_reason(
        command,
        dialect,
        policy,
        cwd,
        &[root()],
        Some(&home()),
        None,
    )
}

fn targets(command: &str, dialect: ShellDialect) -> Vec<CdTarget> {
    scan_session_cd(command, dialect, &root(), Some(&home()))
        .into_iter()
        .map(|occurrence| occurrence.target)
        .collect()
}

/// The `cd` operands as written, for assertions that must not depend on how
/// the host resolves a path spelling.
fn raw_targets(command: &str, dialect: ShellDialect) -> Vec<Option<String>> {
    scan_session_cd(command, dialect, &root(), Some(&home()))
        .into_iter()
        .map(|occurrence| occurrence.raw_target)
        .collect()
}

// -----------------------------------------------------------------
// Scanner: only a `cd` that moves the *session* counts.
// -----------------------------------------------------------------

#[test]
fn a_top_level_cd_is_session_mutating() {
    assert_eq!(targets("cd /tmp", ShellDialect::Posix).len(), 1);
    assert_eq!(targets("ls && cd /tmp", ShellDialect::Posix).len(), 1);
    assert_eq!(targets("cd /tmp; ls", ShellDialect::Posix).len(), 1);
    // Env assignments precede the command word without hiding it.
    assert_eq!(targets("FOO=1 cd /tmp", ShellDialect::Posix).len(), 1);
}

#[test]
fn a_subshell_cd_cannot_leak_and_is_ignored() {
    // The sanctioned workaround the denial message recommends: it must not
    // itself be denied, or the guard would have no escape hatch.
    assert!(targets("(cd /tmp && ls)", ShellDialect::Posix).is_empty());
    assert!(targets("ls; (cd /tmp && make)", ShellDialect::Posix).is_empty());
    assert!(targets("echo $(cd /tmp && pwd)", ShellDialect::Posix).is_empty());
    assert!(targets("echo `cd /tmp && pwd`", ShellDialect::Posix).is_empty());
}

#[test]
fn a_nested_shell_cd_changes_only_the_child() {
    assert!(targets("bash -c 'cd /tmp && ls'", ShellDialect::Posix).is_empty());
    assert!(targets("sh -c \"cd /tmp\"", ShellDialect::Posix).is_empty());
}

#[test]
fn cd_inside_quotes_or_a_heredoc_body_is_not_a_cd() {
    assert!(targets("echo \"cd /tmp\"", ShellDialect::Posix).is_empty());
    assert!(targets("echo 'cd /tmp'", ShellDialect::Posix).is_empty());
    let heredoc = "cat <<'EOF'\ncd /tmp\nEOF";
    assert!(targets(heredoc, ShellDialect::Posix).is_empty());
}

#[test]
fn a_cd_must_be_in_command_position() {
    // The pre-filter is loose by design; the scanner is not. `curl -sL` must
    // not read as PowerShell's `sl`, and a path ending in `cd` is not a `cd`.
    assert!(targets("curl -sL https://example.com", ShellDialect::PowerShell).is_empty());
    assert!(targets("./abcd --flag", ShellDialect::Posix).is_empty());
    assert!(targets("git log --format=cd", ShellDialect::Posix).is_empty());
}

#[test]
fn the_prefilter_admits_every_dialects_builtins() {
    assert!(command_may_change_directory("cd /tmp", ShellDialect::Posix));
    assert!(command_may_change_directory(
        "Set-Location C:\\tmp",
        ShellDialect::PowerShell
    ));
    assert!(command_may_change_directory(
        "sl C:\\tmp",
        ShellDialect::PowerShell
    ));
    assert!(!command_may_change_directory(
        "curl -sL https://example.com",
        ShellDialect::PowerShell
    ));
    assert!(!command_may_change_directory(
        "cargo build",
        ShellDialect::Posix
    ));
}

// -----------------------------------------------------------------
// Target resolution.
// -----------------------------------------------------------------

#[test]
fn home_spellings_resolve_because_they_are_how_agents_leave_a_repo() {
    assert_eq!(
        targets("cd ~", ShellDialect::Posix),
        vec![CdTarget::Path(home())]
    );
    assert_eq!(
        targets("cd $HOME", ShellDialect::Posix),
        vec![CdTarget::Path(home())]
    );
    assert_eq!(
        targets("cd %USERPROFILE%", ShellDialect::Cmd),
        vec![CdTarget::Path(home())]
    );
    assert_eq!(
        targets("cd $HOME/projects", ShellDialect::Posix),
        vec![CdTarget::Path(home().join("projects"))]
    );
}

#[test]
fn an_unknowable_target_is_reported_as_such_not_guessed() {
    assert_eq!(
        targets("cd -", ShellDialect::Posix),
        vec![CdTarget::Unresolvable]
    );
    assert_eq!(
        targets("cd $SOME_DIR", ShellDialect::Posix),
        vec![CdTarget::Unresolvable]
    );
}

#[test]
fn a_bare_cd_differs_by_dialect() {
    // POSIX goes home; PowerShell's no-arg Set-Location and cmd's `cd` do
    // not move at all (verified on Windows PowerShell 5.1).
    assert_eq!(
        targets("cd", ShellDialect::Posix),
        vec![CdTarget::Path(home())]
    );
    assert_eq!(
        targets("cd", ShellDialect::PowerShell),
        vec![CdTarget::NoOp]
    );
    assert_eq!(targets("cd", ShellDialect::Cmd), vec![CdTarget::NoOp]);
}

#[test]
fn flags_are_skipped_to_find_the_operand() {
    // Asserted on the operand *as written*, because what it resolves to is
    // host-dependent: `C:\tmp` is absolute on Windows but a relative name on
    // Linux, and `/tmp` the reverse. Resolution itself is covered above.
    assert_eq!(
        raw_targets("cd -P /tmp", ShellDialect::Posix),
        vec![Some("/tmp".to_string())]
    );
    assert_eq!(
        raw_targets("cd /d C:\\tmp", ShellDialect::Cmd),
        vec![Some("C:\\tmp".to_string())]
    );
    assert_eq!(
        raw_targets("Set-Location -Path C:\\tmp", ShellDialect::PowerShell),
        vec![Some("C:\\tmp".to_string())]
    );
    // A cmd `/d` flag must not be mistaken for a POSIX absolute operand.
    assert_eq!(
        raw_targets("cd /d", ShellDialect::Cmd),
        vec![None],
        "`/d` alone leaves no operand"
    );
}

// -----------------------------------------------------------------
// The decision table.
// -----------------------------------------------------------------

#[test]
fn strict_denies_drift_within_the_repo_because_hooks_break_on_any_drift() {
    // The wedge that motivated the issue was an *in-repo* cd.
    assert!(deny("cd src", CdPolicy::Strict).is_some());
    assert!(deny("cd ./crates/clud-bin", CdPolicy::Strict).is_some());
}

#[test]
fn strict_allows_cd_back_to_a_registered_root_as_the_recovery_path() {
    let spelled_absolute = format!("cd {}", root().display());
    assert!(deny(&spelled_absolute, CdPolicy::Strict).is_none());
    // Spelled relatively from a subdirectory, it is the same directory.
    let subdir = root().join("src");
    assert!(deny_in("cd ..", CdPolicy::Strict, ShellDialect::Posix, &subdir).is_none());
}

#[test]
fn strict_denies_an_unresolvable_target_but_escape_only_does_not() {
    // Strict must be able to prove the destination is a root; escape-only
    // narrows only on evidence of an escape, and has none here.
    assert!(deny("cd $SOME_DIR", CdPolicy::Strict).is_some());
    assert!(deny("cd $SOME_DIR", CdPolicy::EscapeOnly).is_none());
    assert!(deny("cd -", CdPolicy::Strict).is_some());
    assert!(deny("cd -", CdPolicy::EscapeOnly).is_none());
}

#[test]
fn escape_only_allows_in_repo_movement_and_denies_leaving() {
    assert!(deny("cd src", CdPolicy::EscapeOnly).is_none());
    assert!(deny("cd ./crates", CdPolicy::EscapeOnly).is_none());
    assert!(deny("cd ~", CdPolicy::EscapeOnly).is_some());
    assert!(deny("cd ../elsewhere", CdPolicy::EscapeOnly).is_some());
}

#[test]
fn a_bare_cd_that_cannot_move_is_never_denied() {
    assert!(deny_in("cd", CdPolicy::Strict, ShellDialect::PowerShell, &root()).is_none());
    assert!(deny_in("cd", CdPolicy::Strict, ShellDialect::Cmd, &root()).is_none());
    // POSIX bare `cd` does move — to home, which is an escape.
    assert!(deny_in("cd", CdPolicy::Strict, ShellDialect::Posix, &root()).is_some());
}

#[test]
fn policy_off_and_an_empty_root_set_both_decide_nothing() {
    assert!(deny("cd /tmp", CdPolicy::Off).is_none());
    assert!(cd_denial_reason(
        "cd /tmp",
        ShellDialect::Posix,
        CdPolicy::Strict,
        &root(),
        &[],
        Some(&home()),
        None,
    )
    .is_none());
}

#[test]
fn the_denial_names_the_fix_the_recovery_path_and_the_override() {
    let reason = deny("cd src", CdPolicy::Strict).expect("denied");
    assert!(reason.contains("(cd DIR && CMD)"), "{reason}");
    assert!(reason.contains("git -C DIR"), "{reason}");
    assert!(reason.contains("bash.block_cd"), "{reason}");
    assert!(reason.contains(BLOCK_CD_RULE_ID), "{reason}");
    assert!(reason.contains("recover"), "{reason}");
}

#[test]
fn a_strict_denial_quotes_the_hook_that_earned_it() {
    let reason = cd_denial_reason(
        "cd src",
        ShellDialect::Posix,
        CdPolicy::Strict,
        &root(),
        &[root()],
        Some(&home()),
        Some("`uv run python ci/hooks/check.py` in .claude/settings.json"),
    )
    .expect("denied");
    assert!(reason.contains("ci/hooks/check.py"), "{reason}");
}

// -----------------------------------------------------------------
// `"auto"` resolution.
// -----------------------------------------------------------------

fn scan_with(sensitive: bool, any: bool) -> HookCwdScan {
    HookCwdScan {
        any_hooks: any,
        sensitive: if sensitive {
            vec![SensitiveHook {
                source: PathBuf::from(".claude/settings.json"),
                command: "uv run python ci/hooks/check.py".to_string(),
            }]
        } else {
            Vec::new()
        },
        broken_git_prefix: Vec::new(),
    }
}

#[test]
fn auto_resolves_against_the_hooks_actually_in_scope() {
    let sensitive = scan_with(true, true);
    let safe = scan_with(false, true);
    let none = scan_with(false, false);

    assert_eq!(
        resolve_policy(BlockCd::Auto, true, &sensitive),
        CdPolicy::Strict
    );
    assert_eq!(
        resolve_policy(BlockCd::Auto, true, &safe),
        CdPolicy::EscapeOnly
    );
    assert_eq!(resolve_policy(BlockCd::Auto, true, &none), CdPolicy::Off);
    // Outside a repo there is no root to pin to.
    assert_eq!(
        resolve_policy(BlockCd::Auto, false, &sensitive),
        CdPolicy::Off
    );
}

#[test]
fn explicit_settings_do_not_consult_the_hooks() {
    let none = scan_with(false, false);
    assert_eq!(
        resolve_policy(BlockCd::Always, true, &none),
        CdPolicy::Strict
    );
    assert_eq!(
        resolve_policy(BlockCd::Never, true, &scan_with(true, true)),
        CdPolicy::Off
    );
    assert_eq!(resolve_policy(BlockCd::Always, false, &none), CdPolicy::Off);
}

// -----------------------------------------------------------------
// Hook cwd-sensitivity classification.
// -----------------------------------------------------------------

#[test]
fn a_path_binary_or_absolute_command_is_cwd_immune() {
    assert!(!is_cwd_sensitive_hook_command("clud-cmd-scan"));
    assert!(!is_cwd_sensitive_hook_command(
        "clud-cmd-scan; exit $LASTEXITCODE"
    ));
    assert!(!is_cwd_sensitive_hook_command("/usr/local/bin/guard.sh"));
    assert!(!is_cwd_sensitive_hook_command("python -c \"import sys\""));
}

#[test]
fn a_relative_script_path_is_cwd_sensitive() {
    assert!(is_cwd_sensitive_hook_command(
        "uv run python ci/hooks/check-on-stop.py"
    ));
    assert!(is_cwd_sensitive_hook_command("./scripts/guard.sh"));
    assert!(is_cwd_sensitive_hook_command("bash .claude/hooks/lint.sh"));
}

#[test]
fn a_self_rooting_prefix_makes_the_relative_path_safe() {
    assert!(!is_cwd_sensitive_hook_command(
        "cd \"$CLAUDE_PROJECT_DIR\" && uv run python ci/hooks/check-on-stop.py"
    ));
    assert!(!is_cwd_sensitive_hook_command(
        "cd \"$CLUD_PROJECT_DIR\" && ./scripts/guard.sh"
    ));
}

#[test]
fn the_broken_git_rev_parse_prefix_is_detected_and_stays_sensitive() {
    let broken = "cd \"$(git rev-parse --show-superproject-working-tree 2>/dev/null || git \
                  rev-parse --show-toplevel 2>/dev/null || echo .)\" && uv run python \
                  ci/hooks/check-on-stop.py";
    assert!(has_broken_git_rev_parse_prefix(broken));
    // It *looks* self-rooting, which is exactly why it wedged a session: the
    // `||` fallback is dead code, so the relative path is still exposed.
    assert!(is_cwd_sensitive_hook_command(broken));
    assert!(!has_broken_git_rev_parse_prefix(
        "cd \"$CLAUDE_PROJECT_DIR\" && uv run python ci/hooks/check-on-stop.py"
    ));
}

// -----------------------------------------------------------------
// Scanning real config files — the issue's exit criteria.
// -----------------------------------------------------------------

fn write_json(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
    fs::write(path, body).expect("write");
}

#[test]
fn a_stop_hook_is_scanned_even_though_hook_health_reads_only_pretooluse() {
    // The FastLED wedge came from a Stop hook; a PreToolUse-only scan would
    // have resolved `"auto"` to escape-only and allowed the cd that wedged it.
    let tmp = TempDir::new().unwrap();
    write_json(
        &tmp.path().join(".claude").join("settings.json"),
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"uv run python ci/hooks/check-on-stop.py"}]}]}}"#,
    );
    let scan = scan_hook_cwd_sensitivity(tmp.path(), None);
    assert!(scan.any_hooks);
    assert_eq!(scan.sensitive.len(), 1);
    assert_eq!(resolve_policy(BlockCd::Auto, true, &scan), CdPolicy::Strict);
    let hint = scan.hint().expect("hint");
    assert!(hint.contains("check-on-stop.py"), "{hint}");
}

#[test]
fn a_repo_whose_hooks_are_all_path_binaries_resolves_to_escape_only() {
    let tmp = TempDir::new().unwrap();
    write_json(
        &tmp.path().join(".claude").join("settings.json"),
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"clud-cmd-scan"}]}]}}"#,
    );
    let scan = scan_hook_cwd_sensitivity(tmp.path(), None);
    assert!(scan.any_hooks);
    assert!(scan.sensitive.is_empty());
    assert_eq!(
        resolve_policy(BlockCd::Auto, true, &scan),
        CdPolicy::EscapeOnly
    );
}

#[test]
fn a_repo_with_no_hooks_gets_no_policy_at_all() {
    // Exit criterion: repos with no hooks see no behavior change.
    let tmp = TempDir::new().unwrap();
    let scan = scan_hook_cwd_sensitivity(tmp.path(), None);
    assert_eq!(scan, HookCwdScan::default());
    assert_eq!(resolve_policy(BlockCd::Auto, true, &scan), CdPolicy::Off);
}

#[test]
fn codex_hook_configs_are_scanned_too() {
    let tmp = TempDir::new().unwrap();
    write_json(
        &tmp.path().join(".codex").join("hooks.json"),
        r#"{"hooks":{"PreToolUse":[{"hooks":[{"command":"uv run python ci/hooks/guard.py"}]}]}}"#,
    );
    let scan = scan_hook_cwd_sensitivity(tmp.path(), None);
    assert_eq!(scan.sensitive.len(), 1);
}

#[test]
fn the_broken_prefix_is_recorded_with_its_source_for_the_hooks_report() {
    let tmp = TempDir::new().unwrap();
    write_json(
        &tmp.path().join(".claude").join("settings.json"),
        r#"{"hooks":{"Stop":[{"hooks":[{"command":"cd \"$(git rev-parse --show-superproject-working-tree 2>/dev/null || git rev-parse --show-toplevel 2>/dev/null || echo .)\" && uv run python ci/hooks/check.py"}]}]}}"#,
    );
    let scan = scan_hook_cwd_sensitivity(tmp.path(), None);
    assert_eq!(scan.broken_git_prefix.len(), 1);
    assert_eq!(scan.sensitive.len(), 1, "broken prefix is still sensitive");
}

#[test]
fn an_unparsable_hook_config_is_skipped_rather_than_fatal() {
    let tmp = TempDir::new().unwrap();
    write_json(
        &tmp.path().join(".claude").join("settings.json"),
        "{not json",
    );
    assert_eq!(
        scan_hook_cwd_sensitivity(tmp.path(), None),
        HookCwdScan::default()
    );
}

// -----------------------------------------------------------------
// Repo-root discovery.
// -----------------------------------------------------------------

#[test]
fn nearest_repo_root_distinguishes_no_repo_from_repo_at_cwd() {
    let tmp = TempDir::new().unwrap();
    let nested = tmp.path().join("crates").join("clud-bin");
    fs::create_dir_all(&nested).unwrap();
    assert_eq!(nearest_repo_root(&nested), None);

    fs::create_dir_all(tmp.path().join(".git")).unwrap();
    let found = nearest_repo_root(&nested).expect("root");
    assert_eq!(
        crate::path_norm::normalize_for_key(&found),
        crate::path_norm::normalize_for_key(tmp.path())
    );
}

#[test]
fn a_git_file_counts_so_linked_worktrees_resolve() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join(".git"),
        "gitdir: /elsewhere/.git/worktrees/wt",
    )
    .unwrap();
    assert!(nearest_repo_root(tmp.path()).is_some());
}
