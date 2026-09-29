//! Rollout helpers for the native `cmd-scan` hook binary (formerly
//! `block-bad-cmd`).
//!
//! #489/#490 moved the command-guard policy into a native helper, but older
//! installs and hook configs can still route through
//! `clud tool run hooks/block-bad-cmd.py`. #532 renamed the native helper
//! itself from `clud-block-bad-cmd` to `clud-cmd-scan` (it now also does
//! eager GC tracking of `git clone`/`git worktree add`, not just command
//! blocking), so there are now two legacy command shapes to migrate away
//! from. This module owns the narrow first-run repair path: detect an
//! installed clud missing the helper and rewrite only the exact old managed
//! hook commands once the sibling helper is available.
//!
//! Project-scoped configs (`<repo>/.claude/settings*.json`,
//! `<repo>/.codex/hooks.json`) are usually committed and shared, so they are
//! never pinned to the launching executable's absolute path: that path exists
//! on one machine only, and writing it dirtied tracked files every time a dev
//! build or the test suite launched clud inside a checkout (#1333, #1426).
//! There, only the legacy command shapes are rewritten, to the portable bare
//! `clud-cmd-scan`. User-scoped configs under the hook home keep #1279's
//! pinning.

use serde_json::Value;
use std::io;
use std::path::{Path, PathBuf};

const LEGACY_PYTHON_SHIM_COMMAND: &str = "clud tool run hooks/block-bad-cmd.py";
const LEGACY_PYTHON_SHIM_COMMAND_EXIT: &str =
    "clud tool run hooks/block-bad-cmd.py; exit $LASTEXITCODE";
const LEGACY_NATIVE_COMMAND: &str = "clud-block-bad-cmd";
const LEGACY_NATIVE_COMMAND_EXIT: &str = "clud-block-bad-cmd; exit $LASTEXITCODE";
const NEW_COMMAND: &str = "clud-cmd-scan";
const NEW_COMMAND_EXIT: &str = "clud-cmd-scan; exit $LASTEXITCODE";
const PINNED_PYTHON_SHIM_COMMAND: &str = "\"$CLUD_EXE\" tool run hooks/block-bad-cmd.py";
const PINNED_PYTHON_SHIM_COMMAND_EXIT: &str =
    "& $env:CLUD_EXE tool run hooks/block-bad-cmd.py; exit $LASTEXITCODE";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallProbe {
    HelperPresent { path: PathBuf },
    MissingFromInstalledLayout { expected: PathBuf },
    NotInstalledLayout,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MigrationReport {
    pub files_changed: usize,
    pub commands_changed: usize,
    pub stale_commands_blocked: usize,
}

/// Where a hook config lives, which decides whether it may be pinned to the
/// launching executable (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigScope {
    /// Inside the repo; usually committed, so only portable rewrites.
    Project,
    /// Under the hook home; machine-local, so pinning is safe.
    User,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct FileMigration {
    commands_changed: usize,
    stale_commands_blocked: usize,
}

pub fn run_startup_checks(auto_fix_hooks: bool) {
    let sibling_helper_present =
        matches!(probe_current_install(), InstallProbe::HelperPresent { .. });
    if !sibling_helper_present {
        if let InstallProbe::MissingFromInstalledLayout { expected } = probe_current_install() {
            eprintln!(
                "[clud] warning: native hook helper `{}` is missing at {}; run `uv tool install --force clud` to repair this install",
                native_helper_name(),
                expected.display()
            );
        }
    }

    if !auto_fix_hooks {
        return;
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let repo_root = crate::loop_spec::git_root_from(&cwd);
    let home = hook_home_dir();
    match migrate_hook_configs_at(&repo_root, home.as_deref(), sibling_helper_present) {
        Ok(report) => {
            if report.commands_changed > 0 {
                if !sibling_helper_present {
                    eprintln!("[clud] migrated legacy hook command to pinned CLUD_EXE shim");
                } else {
                    eprintln!(
                    "\x1b[32m[clud] migrated {count} block-bad-cmd hook command{plural} to native `{helper}`\x1b[0m",
                    count = report.commands_changed,
                    plural = if report.commands_changed == 1 { "" } else { "s" },
                    helper = NEW_COMMAND,
                );
                }
            }
            if report.stale_commands_blocked > 0 {
                eprintln!(
                    "[clud] warning: found {count} stale block-bad-cmd hook command{plural}, but `{helper}` is not on PATH; leaving compatibility shim wiring in place",
                    count = report.stale_commands_blocked,
                    plural = if report.stale_commands_blocked == 1 { "" } else { "s" },
                    helper = NEW_COMMAND,
                );
            }
        }
        Err(error) => {
            eprintln!("[clud] warning: failed to migrate block-bad-cmd hook config: {error}");
        }
    }
}

pub fn probe_current_install() -> InstallProbe {
    match std::env::current_exe() {
        Ok(path) => probe_install_at(&path),
        Err(_) => InstallProbe::NotInstalledLayout,
    }
}

pub fn probe_install_at(current_exe: &Path) -> InstallProbe {
    let Some(parent) = current_exe.parent() else {
        return InstallProbe::NotInstalledLayout;
    };
    let helper = parent.join(native_helper_name());
    if helper.is_file() {
        return InstallProbe::HelperPresent { path: helper };
    }

    let shim = parent.join(native_binary_name("clud-shim"));
    if shim.is_file() {
        return InstallProbe::MissingFromInstalledLayout { expected: helper };
    }

    InstallProbe::NotInstalledLayout
}

pub fn native_helper_name() -> &'static str {
    native_binary_name("clud-cmd-scan")
}

fn native_binary_name(name: &'static str) -> &'static str {
    #[cfg(windows)]
    {
        match name {
            "clud" => "clud.exe",
            "clud-cmd-scan" => "clud-cmd-scan.exe",
            "clud-shim" => "clud-shim.exe",
            _ => name,
        }
    }
    #[cfg(not(windows))]
    {
        name
    }
}

pub fn migrate_hook_configs_at(
    repo_root: &Path,
    home: Option<&Path>,
    helper_available: bool,
) -> io::Result<MigrationReport> {
    let mut report = MigrationReport::default();
    for (path, scope) in hook_config_paths(repo_root, home) {
        if !path.is_file() {
            continue;
        }
        let outcome = migrate_file(&path, helper_available, scope)?;
        if outcome.commands_changed > 0 {
            report.files_changed += 1;
            report.commands_changed += outcome.commands_changed;
        }
        report.stale_commands_blocked += outcome.stale_commands_blocked;
    }
    Ok(report)
}

const PROJECT_CONFIGS: [(&str, &str); 3] = [
    (".claude", "settings.json"),
    (".claude", "settings.local.json"),
    (".codex", "hooks.json"),
];
const USER_CONFIGS: [(&str, &str); 2] = [(".claude", "settings.json"), (".codex", "hooks.json")];

fn hook_config_paths(repo_root: &Path, home: Option<&Path>) -> Vec<(PathBuf, ConfigScope)> {
    let mut paths = Vec::new();
    for (dir, file) in PROJECT_CONFIGS {
        paths.push((repo_root.join(dir).join(file), ConfigScope::Project));
    }
    if let Some(home) = home {
        for (dir, file) in USER_CONFIGS {
            paths.push((home.join(dir).join(file), ConfigScope::User));
        }
    }
    paths
}

fn migrate_file(
    path: &Path,
    helper_available: bool,
    scope: ConfigScope,
) -> io::Result<FileMigration> {
    let text = std::fs::read_to_string(path)?;
    let mut json: Value = match serde_json::from_str(&text) {
        Ok(json) => json,
        Err(_) => return Ok(FileMigration::default()),
    };

    let stale = count_stale_commands(&json, scope);
    if stale == 0 {
        return Ok(FileMigration::default());
    }
    if !helper_available && !has_legacy_python_shim(&json) {
        return Ok(FileMigration {
            commands_changed: 0,
            stale_commands_blocked: stale,
        });
    }

    let mut changed = 0usize;
    migrate_value(&mut json, &mut changed, helper_available, scope);
    if changed == 0 {
        return Ok(FileMigration::default());
    }

    let mut body = serde_json::to_string_pretty(&json).map_err(io::Error::other)?;
    body.push('\n');
    std::fs::write(path, body)?;
    Ok(FileMigration {
        commands_changed: changed,
        stale_commands_blocked: count_stale_commands(&json, scope),
    })
}

fn count_stale_commands(value: &Value, scope: ConfigScope) -> usize {
    match value {
        Value::Object(map) => {
            let here = map
                .get("command")
                .and_then(Value::as_str)
                .filter(|command| command_is_stale(command, scope))
                .map(|_| 1)
                .unwrap_or(0);
            let nested: usize = map.values().map(|v| count_stale_commands(v, scope)).sum();
            here + nested
        }
        Value::Array(values) => values.iter().map(|v| count_stale_commands(v, scope)).sum(),
        _ => 0,
    }
}

fn migrate_value(
    value: &mut Value,
    changed: &mut usize,
    helper_available: bool,
    scope: ConfigScope,
) {
    match value {
        Value::Object(map) => {
            let replacement = map
                .get("command")
                .and_then(Value::as_str)
                .and_then(|command| replacement_command_for(command, helper_available, scope));
            if let Some(replacement) = replacement {
                map.insert("command".to_string(), Value::String(replacement));
                *changed += 1;
            }
            for value in map.values_mut() {
                migrate_value(value, changed, helper_available, scope);
            }
        }
        Value::Array(values) => {
            for value in values {
                migrate_value(value, changed, helper_available, scope);
            }
        }
        _ => {}
    }
}

fn command_is_stale(command: &str, scope: ConfigScope) -> bool {
    replacement_command(command, scope).is_some()
}

fn has_legacy_python_shim(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.get("command")
                .and_then(Value::as_str)
                .is_some_and(|command| {
                    matches!(
                        command,
                        LEGACY_PYTHON_SHIM_COMMAND | LEGACY_PYTHON_SHIM_COMMAND_EXIT
                    )
                })
                || map.values().any(has_legacy_python_shim)
        }
        Value::Array(values) => values.iter().any(has_legacy_python_shim),
        _ => false,
    }
}

fn replacement_command_for(
    command: &str,
    helper_available: bool,
    scope: ConfigScope,
) -> Option<String> {
    if helper_available && scope == ConfigScope::Project {
        // Portable only: never write this machine's helper path into a
        // (usually committed) project file (#1333, #1426).
        return replacement_command(command, scope).map(str::to_string);
    }
    if helper_available {
        let parent = std::env::current_exe().ok()?.parent()?.to_path_buf();
        let helper = parent.join(native_helper_name());
        let helper = helper.to_str()?;
        return match command {
            LEGACY_PYTHON_SHIM_COMMAND | LEGACY_NATIVE_COMMAND | NEW_COMMAND => {
                if cfg!(windows) {
                    Some(format!("& '{}'", helper.replace('\'', "''")))
                } else {
                    Some(format!("'{}'", helper.replace('\'', "'\\''")))
                }
            }
            LEGACY_PYTHON_SHIM_COMMAND_EXIT | LEGACY_NATIVE_COMMAND_EXIT | NEW_COMMAND_EXIT => {
                Some(format!(
                    "& '{}'; exit $LASTEXITCODE",
                    helper.replace('\'', "''")
                ))
            }
            _ => None,
        };
    }
    match command {
        LEGACY_PYTHON_SHIM_COMMAND => Some(PINNED_PYTHON_SHIM_COMMAND.to_string()),
        LEGACY_PYTHON_SHIM_COMMAND_EXIT => Some(PINNED_PYTHON_SHIM_COMMAND_EXIT.to_string()),
        _ => None,
    }
}

/// The portable command a stale one migrates to, or `None` when `command` is
/// current. The bare helper is current in a project file (it must stay
/// portable) but stale in a user file, where it gets pinned (#1279).
fn replacement_command(command: &str, scope: ConfigScope) -> Option<&'static str> {
    if scope == ConfigScope::Project && matches!(command, NEW_COMMAND | NEW_COMMAND_EXIT) {
        return None;
    }
    match command {
        LEGACY_PYTHON_SHIM_COMMAND | LEGACY_NATIVE_COMMAND | NEW_COMMAND => Some(NEW_COMMAND),
        LEGACY_PYTHON_SHIM_COMMAND_EXIT | LEGACY_NATIVE_COMMAND_EXIT | NEW_COMMAND_EXIT => {
            Some(NEW_COMMAND_EXIT)
        }
        _ => None,
    }
}

fn hook_home_dir() -> Option<PathBuf> {
    std::env::var_os("CLUD_HOOK_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    /// What a user-scoped command is pinned to by this test binary.
    fn pinned(command: &str) -> String {
        replacement_command_for(command, true, ConfigScope::User).unwrap()
    }

    #[test]
    fn installed_layout_probe_detects_missing_helper() {
        let tmp = tempdir().unwrap();
        let bin = tmp.path();
        let clud = bin.join(native_binary_name("clud"));
        let shim = bin.join(native_binary_name("clud-shim"));
        write(&clud, "");
        write(&shim, "");

        assert_eq!(
            probe_install_at(&clud),
            InstallProbe::MissingFromInstalledLayout {
                expected: bin.join(native_helper_name())
            }
        );

        write(&bin.join(native_helper_name()), "");
        assert_eq!(
            probe_install_at(&clud),
            InstallProbe::HelperPresent {
                path: bin.join(native_helper_name())
            }
        );
    }

    #[test]
    fn copied_test_binary_without_shim_is_not_installed_layout() {
        let tmp = tempdir().unwrap();
        let clud = tmp.path().join(native_binary_name("clud"));
        write(&clud, "");

        assert_eq!(probe_install_at(&clud), InstallProbe::NotInstalledLayout);
    }

    #[test]
    fn migrates_exact_claude_and_codex_commands_when_helper_available() {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("home");
        write(
            &home.join(".claude/settings.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"clud tool run hooks/block-bad-cmd.py"}]}]}}"#,
        );
        write(
            &home.join(".codex/hooks.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"clud tool run hooks/block-bad-cmd.py; exit $LASTEXITCODE"}]}]}}"#,
        );

        let report = migrate_hook_configs_at(&repo, Some(&home), true).unwrap();

        assert_eq!(report.files_changed, 2);
        assert_eq!(report.commands_changed, 2);
        assert_eq!(report.stale_commands_blocked, 0);
        let claude: Value =
            serde_json::from_str(&fs::read_to_string(home.join(".claude/settings.json")).unwrap())
                .unwrap();
        let codex: Value =
            serde_json::from_str(&fs::read_to_string(home.join(".codex/hooks.json")).unwrap())
                .unwrap();
        assert_eq!(
            claude["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            pinned(LEGACY_PYTHON_SHIM_COMMAND)
        );
        assert_eq!(
            codex["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            pinned(LEGACY_PYTHON_SHIM_COMMAND_EXIT)
        );

        let second = migrate_hook_configs_at(&repo, Some(&home), true).unwrap();
        assert_eq!(second, MigrationReport::default());
    }

    #[test]
    fn migrates_legacy_native_block_bad_cmd_command_to_cmd_scan() {
        // #532: a hook config already migrated once (python shim ->
        // `clud-block-bad-cmd`) must also be carried forward to the
        // renamed `clud-cmd-scan` binary.
        let tmp = tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("home");
        write(
            &home.join(".claude/settings.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"clud-block-bad-cmd"}]}]}}"#,
        );
        write(
            &home.join(".codex/hooks.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"clud-block-bad-cmd; exit $LASTEXITCODE"}]}]}}"#,
        );

        let report = migrate_hook_configs_at(&repo, Some(&home), true).unwrap();

        assert_eq!(report.files_changed, 2);
        assert_eq!(report.commands_changed, 2);
        assert_eq!(report.stale_commands_blocked, 0);

        let claude: Value =
            serde_json::from_str(&fs::read_to_string(home.join(".claude/settings.json")).unwrap())
                .unwrap();
        let codex: Value =
            serde_json::from_str(&fs::read_to_string(home.join(".codex/hooks.json")).unwrap())
                .unwrap();
        assert_eq!(
            claude["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            pinned(LEGACY_NATIVE_COMMAND)
        );
        assert_eq!(
            codex["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            pinned(LEGACY_NATIVE_COMMAND_EXIT)
        );
    }

    #[test]
    fn missing_helper_blocks_rewrite_of_legacy_native_command() {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("home");
        let path = home.join(".claude/settings.json");
        write(
            &path,
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"command":"clud-block-bad-cmd"}]}]}}"#,
        );

        let report = migrate_hook_configs_at(&repo, Some(&home), false).unwrap();

        assert_eq!(report.files_changed, 0);
        assert_eq!(report.commands_changed, 0);
        assert_eq!(report.stale_commands_blocked, 1);
        assert!(fs::read_to_string(path)
            .unwrap()
            .contains("clud-block-bad-cmd"));
    }

    #[test]
    fn already_migrated_bare_helper_is_pinned_to_launching_install() {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("home");
        let path = home.join(".claude/settings.json");
        write(&path, r#"{"hooks":[{"command":"clud-cmd-scan"}]}"#);

        let report = migrate_hook_configs_at(&repo, Some(&home), true).unwrap();

        assert_eq!(report.commands_changed, 1);
        let config: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(config["hooks"][0]["command"], pinned(NEW_COMMAND));
    }

    #[test]
    fn missing_helper_blocks_rewrite_and_preserves_compatibility_command() {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("home");
        let path = home.join(".claude/settings.json");
        write(
            &path,
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"command":"clud tool run hooks/block-bad-cmd.py"}]}]}}"#,
        );

        let report = migrate_hook_configs_at(&repo, Some(&home), false).unwrap();

        assert_eq!(report.files_changed, 1);
        assert_eq!(report.commands_changed, 1);
        assert_eq!(report.stale_commands_blocked, 0);
        let config: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(
            config["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            PINNED_PYTHON_SHIM_COMMAND
        );
    }

    #[test]
    fn non_exact_user_variants_are_untouched() {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("home");
        let body = r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"command":"python wrapper.py && clud tool run hooks/block-bad-cmd.py"},{"command":" clud tool run hooks/block-bad-cmd.py "}]}]}}"#;
        let path = home.join(".claude/settings.json");
        write(&path, body);

        let report = migrate_hook_configs_at(&repo, Some(&home), true).unwrap();

        assert_eq!(report, MigrationReport::default());
        assert_eq!(fs::read_to_string(path).unwrap(), body);
    }

    /// This repo's own committed hook configs, as a launch inside the
    /// checkout sees them (#1333, #1426).
    const COMMITTED_CLAUDE_SETTINGS: &str = include_str!("../../../.claude/settings.json");
    const COMMITTED_CODEX_HOOKS: &str = include_str!("../../../.codex/hooks.json");

    const PROJECT_CONFIG_RELS: [&str; 3] = [
        ".claude/settings.json",
        ".claude/settings.local.json",
        ".codex/hooks.json",
    ];

    #[test]
    fn project_bare_helper_is_never_pinned_to_the_launching_install() {
        // #1426: a dev build (or the test suite) launched inside a checkout
        // rewrote the committed `clud-cmd-scan` to its own absolute path.
        let tmp = tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("home");
        let body = r#"{"hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"command":"clud-cmd-scan"},{"command":"clud-cmd-scan; exit $LASTEXITCODE"}]}]}}"#;
        for rel in PROJECT_CONFIG_RELS {
            write(&repo.join(rel), body);
        }

        for helper_available in [true, false] {
            let report = migrate_hook_configs_at(&repo, Some(&home), helper_available).unwrap();
            assert_eq!(report, MigrationReport::default());
        }
        for rel in PROJECT_CONFIG_RELS {
            assert_eq!(fs::read_to_string(repo.join(rel)).unwrap(), body, "{rel}");
        }
    }

    #[test]
    fn project_legacy_commands_migrate_to_the_portable_bare_helper() {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let path = repo.join(".claude/settings.json");
        write(
            &path,
            r#"{"hooks":[{"command":"clud tool run hooks/block-bad-cmd.py"},{"command":"clud-block-bad-cmd; exit $LASTEXITCODE"}]}"#,
        );

        let report = migrate_hook_configs_at(&repo, None, true).unwrap();

        assert_eq!(report.files_changed, 1);
        assert_eq!(report.commands_changed, 2);
        assert_eq!(report.stale_commands_blocked, 0);
        let config: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(config["hooks"][0]["command"], NEW_COMMAND);
        assert_eq!(config["hooks"][1]["command"], NEW_COMMAND_EXIT);
    }

    #[test]
    fn committed_repo_hook_configs_use_the_portable_bare_helper() {
        // #1333 and #1428 both committed a machine-local helper path.
        for (name, body) in [
            (".claude/settings.json", COMMITTED_CLAUDE_SETTINGS),
            (".codex/hooks.json", COMMITTED_CODEX_HOOKS),
        ] {
            let json: Value = serde_json::from_str(body).unwrap();
            let mut commands = Vec::new();
            collect_commands(&json, &mut commands);
            let scans: Vec<_> = commands
                .iter()
                .filter(|command| command.contains("clud-cmd-scan"))
                .collect();
            assert!(!scans.is_empty(), "{name} lost its clud-cmd-scan hook");
            for command in scans {
                assert_eq!(command, NEW_COMMAND, "{name} must not pin clud-cmd-scan");
            }
        }
    }

    #[test]
    fn committed_repo_hook_configs_survive_a_launch_unchanged() {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let claude = repo.join(".claude/settings.json");
        let codex = repo.join(".codex/hooks.json");
        write(&claude, COMMITTED_CLAUDE_SETTINGS);
        write(&codex, COMMITTED_CODEX_HOOKS);

        for helper_available in [true, false] {
            let report = migrate_hook_configs_at(&repo, None, helper_available).unwrap();
            assert_eq!(report, MigrationReport::default());
        }
        let claude_after = fs::read_to_string(claude).unwrap();
        let codex_after = fs::read_to_string(codex).unwrap();
        assert_eq!(claude_after, COMMITTED_CLAUDE_SETTINGS);
        assert_eq!(codex_after, COMMITTED_CODEX_HOOKS);
    }

    fn collect_commands(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                if let Some(command) = map.get("command").and_then(Value::as_str) {
                    out.push(command.to_string());
                }
                for value in map.values() {
                    collect_commands(value, out);
                }
            }
            Value::Array(values) => {
                for value in values {
                    collect_commands(value, out);
                }
            }
            _ => {}
        }
    }
}
