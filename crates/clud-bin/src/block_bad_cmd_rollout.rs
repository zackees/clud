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
//! hook commands once the helper is resolvable on PATH.

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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct FileMigration {
    commands_changed: usize,
    stale_commands_blocked: usize,
}

pub fn run_startup_checks(auto_fix_hooks: bool) {
    let helper_on_path = native_helper_on_path().is_some();
    if !helper_on_path {
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
    match migrate_hook_configs_at(&repo_root, home.as_deref(), helper_on_path) {
        Ok(report) => {
            if report.commands_changed > 0 {
                eprintln!(
                    "\x1b[32m[clud] migrated {count} block-bad-cmd hook command{plural} to native `{helper}`\x1b[0m",
                    count = report.commands_changed,
                    plural = if report.commands_changed == 1 { "" } else { "s" },
                    helper = NEW_COMMAND,
                );
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

pub fn native_helper_on_path() -> Option<PathBuf> {
    which::which(native_helper_name()).ok()
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
    for path in hook_config_paths(repo_root, home) {
        if !path.is_file() {
            continue;
        }
        let outcome = migrate_file(&path, helper_available)?;
        if outcome.commands_changed > 0 {
            report.files_changed += 1;
            report.commands_changed += outcome.commands_changed;
        }
        report.stale_commands_blocked += outcome.stale_commands_blocked;
    }
    Ok(report)
}

fn hook_config_paths(repo_root: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = vec![
        repo_root.join(".claude").join("settings.json"),
        repo_root.join(".claude").join("settings.local.json"),
        repo_root.join(".codex").join("hooks.json"),
    ];
    if let Some(home) = home {
        paths.push(home.join(".claude").join("settings.json"));
        paths.push(home.join(".codex").join("hooks.json"));
    }
    paths
}

fn migrate_file(path: &Path, helper_available: bool) -> io::Result<FileMigration> {
    let text = std::fs::read_to_string(path)?;
    let mut json: Value = match serde_json::from_str(&text) {
        Ok(json) => json,
        Err(_) => return Ok(FileMigration::default()),
    };

    let stale = count_stale_commands(&json);
    if stale == 0 {
        return Ok(FileMigration::default());
    }
    if !helper_available {
        return Ok(FileMigration {
            commands_changed: 0,
            stale_commands_blocked: stale,
        });
    }

    let mut changed = 0usize;
    migrate_value(&mut json, &mut changed);
    if changed == 0 {
        return Ok(FileMigration::default());
    }

    let mut body = serde_json::to_string_pretty(&json).map_err(io::Error::other)?;
    body.push('\n');
    std::fs::write(path, body)?;
    Ok(FileMigration {
        commands_changed: changed,
        stale_commands_blocked: 0,
    })
}

fn count_stale_commands(value: &Value) -> usize {
    match value {
        Value::Object(map) => {
            let here = map
                .get("command")
                .and_then(Value::as_str)
                .filter(|command| command_is_stale(command))
                .map(|_| 1)
                .unwrap_or(0);
            here + map.values().map(count_stale_commands).sum::<usize>()
        }
        Value::Array(values) => values.iter().map(count_stale_commands).sum(),
        _ => 0,
    }
}

fn migrate_value(value: &mut Value, changed: &mut usize) {
    match value {
        Value::Object(map) => {
            if let Some(command) = map.get("command").and_then(Value::as_str) {
                if let Some(replacement) = replacement_command(command) {
                    map.insert(
                        "command".to_string(),
                        Value::String(replacement.to_string()),
                    );
                    *changed += 1;
                }
            }
            for value in map.values_mut() {
                migrate_value(value, changed);
            }
        }
        Value::Array(values) => {
            for value in values {
                migrate_value(value, changed);
            }
        }
        _ => {}
    }
}

fn command_is_stale(command: &str) -> bool {
    replacement_command(command).is_some()
}

fn replacement_command(command: &str) -> Option<&'static str> {
    match command {
        LEGACY_PYTHON_SHIM_COMMAND | LEGACY_NATIVE_COMMAND => Some(NEW_COMMAND),
        LEGACY_PYTHON_SHIM_COMMAND_EXIT | LEGACY_NATIVE_COMMAND_EXIT => Some(NEW_COMMAND_EXIT),
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
        let claude = fs::read_to_string(home.join(".claude/settings.json")).unwrap();
        let codex = fs::read_to_string(home.join(".codex/hooks.json")).unwrap();
        assert!(claude.contains(r#""command": "clud-cmd-scan""#), "{claude}");
        assert!(
            codex.contains(r#""command": "clud-cmd-scan; exit $LASTEXITCODE""#),
            "{codex}"
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

        let claude = fs::read_to_string(home.join(".claude/settings.json")).unwrap();
        let codex = fs::read_to_string(home.join(".codex/hooks.json")).unwrap();
        assert!(claude.contains(r#""command": "clud-cmd-scan""#), "{claude}");
        assert!(
            codex.contains(r#""command": "clud-cmd-scan; exit $LASTEXITCODE""#),
            "{codex}"
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

        assert_eq!(report.files_changed, 0);
        assert_eq!(report.commands_changed, 0);
        assert_eq!(report.stale_commands_blocked, 1);
        assert!(fs::read_to_string(path)
            .unwrap()
            .contains("clud tool run hooks/block-bad-cmd.py"));
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
}
