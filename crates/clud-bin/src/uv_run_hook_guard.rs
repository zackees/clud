//! Startup-time invocation of the bundled `uv_run_hook_guard.py` tool.
//!
//! The detection logic lives in Python so the same script can also be
//! run by hand (`clud tool run hooks/uv_run_hook_guard.py`) or as a
//! user-configured SessionStart hook in a different agent. This module
//! is the thin Rust wrapper that fires it on every clud launch.
//!
//! Fast path: the gate at `main.rs` (`args.command.is_none() && ...`)
//! avoids calling this for any subcommand invocation. Inside `run()`
//! we additionally short-circuit when the bundled tool isn't installed
//! yet (cold first-ever clud invocation that hasn't seen the daemon
//! complete its install pass) — silently returning so we never block
//! startup on a missing asset. The daemon's next launch will install
//! the file and the guard becomes active.
//!
//! Subprocess execution goes through `NativeProcess` per the repo-wide
//! ban on direct `std::process::Command` use.

use std::path::{Path, PathBuf};
use std::time::Duration;

use running_process::{CommandSpec, NativeProcess, ProcessConfig, StderrMode, StdinMode};

use crate::tool_install::tools_root;

/// Hard wall-clock cap. The guard's slowest legitimate runtime is
/// "found at least one offender" → ~50ms scan + 3s deliberate sleep =
/// well under 5s. Anything past that is uv hanging on a sync we don't
/// want to wait for at startup.
const DEADLINE: Duration = Duration::from_secs(8);

/// Relative path under `~/.clud/tools/` of the bundled tool.
const TOOL_REL_PATH: &str = "hooks/uv_run_hook_guard.py";

/// Run the guard against `project_root`. Silent on every failure mode
/// (tool not installed, uv missing, subprocess error) so the guard
/// never breaks a launch.
pub fn run(project_root: &Path) {
    let Some(tool_path) = installed_tool_path() else {
        return;
    };
    if !tool_path.exists() {
        return;
    }

    let argv: Vec<String> = vec![
        "uv".to_string(),
        "run".to_string(),
        "--script".to_string(),
        tool_path.to_string_lossy().into_owned(),
        project_root.to_string_lossy().into_owned(),
    ];

    let process = NativeProcess::new(ProcessConfig {
        command: CommandSpec::Argv(argv),
        cwd: Some(project_root.to_path_buf()),
        env: None,
        capture: false,
        stderr_mode: StderrMode::Stdout,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Inherit,
        nice: None,
    });
    if process.start().is_err() {
        return;
    }
    let _ = process.wait(Some(DEADLINE));
}

fn installed_tool_path() -> Option<PathBuf> {
    Some(tools_root()?.join(TOOL_REL_PATH))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guard rel_path is the same string both this wrapper and the
    /// BUNDLED_TOOLS registry rely on. Drift would mean the wrapper
    /// looks for a file the install lifecycle never wrote.
    #[test]
    fn tool_rel_path_matches_bundled_registry() {
        let registered = crate::tools::BUNDLED_TOOLS
            .iter()
            .any(|t| t.rel_path == TOOL_REL_PATH);
        assert!(
            registered,
            "TOOL_REL_PATH `{TOOL_REL_PATH}` is not in BUNDLED_TOOLS — \
             the wrapper would silently never find the asset"
        );
    }

    /// Sanity-check the resolver: when `tools_root` succeeds the path
    /// ends with `hooks/uv_run_hook_guard.py` (or the Windows
    /// equivalent). When it fails (no home dir in env), we return None
    /// rather than producing a nonsense path.
    #[test]
    fn installed_tool_path_shape() {
        let Some(path) = installed_tool_path() else {
            return;
        };
        let s = path.to_string_lossy().to_string();
        assert!(
            s.ends_with("hooks/uv_run_hook_guard.py") || s.ends_with("hooks\\uv_run_hook_guard.py"),
            "installed_tool_path returned an unexpected suffix: {s}"
        );
    }
}
