use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::args::Args;

use super::types::ENV_STATE_DIR;

/// Per-user daemon state dir, used by everything in this module that
/// needs to find the daemon (the args-aware `state_dir` wrapper above,
/// `client::ensure_daemon`, the `clud gc` CLI client).
///
/// Resolution order: `CLUD_DAEMON_STATE_DIR` env var → `~/.clud/state`.
/// Issue #135: pre-merge, the standalone gc_daemon used `~/.clud/state`
/// and the session daemon used `$TMP/clud-daemon`. The merged daemon
/// adopts the persistent location so the registry, info file, lock,
/// and logs all share one directory across reboots.
pub fn default_state_dir() -> std::io::Result<PathBuf> {
    if let Ok(path) = std::env::var(ENV_STATE_DIR) {
        return Ok(PathBuf::from(path));
    }
    let home = clud_home_dir()
        .ok_or_else(|| std::io::Error::other("no home directory; cannot resolve clud state dir"))?;
    Ok(home.join(".clud").join("state"))
}

/// Per-user quarantine root for `clud trash`.
///
/// Lives next to `data.redb` at `~/.clud/trash/` so the always-on daemon
/// can reap entries across all repos and shells.
pub fn default_trash_dir() -> std::io::Result<PathBuf> {
    let home = clud_home_dir()
        .ok_or_else(|| std::io::Error::other("no home directory; cannot resolve clud trash dir"))?;
    Ok(home.join(".clud").join("trash"))
}

fn clud_home_dir() -> Option<PathBuf> {
    home_dir_from_parts(
        std::env::var_os("USERPROFILE"),
        std::env::var_os("HOME"),
        dirs::home_dir(),
    )
}

fn home_dir_from_parts(
    userprofile: Option<OsString>,
    home: Option<OsString>,
    fallback: Option<PathBuf>,
) -> Option<PathBuf> {
    #[cfg(windows)]
    if let Some(path) = non_empty_path(userprofile) {
        return Some(path);
    }

    #[cfg(not(windows))]
    let _ = userprofile;

    non_empty_path(home).or(fallback)
}

fn non_empty_path(value: Option<OsString>) -> Option<PathBuf> {
    value.filter(|path| !path.is_empty()).map(PathBuf::from)
}

pub(super) fn state_dir(args: &Args) -> PathBuf {
    if let Some(path) = &args.daemon_state_dir {
        return path.clone();
    }
    default_state_dir().unwrap_or_else(|_| std::env::temp_dir().join("clud-daemon"))
}

pub(super) fn daemon_info_path(state_dir: &Path) -> PathBuf {
    state_dir.join("daemon.json")
}

/// fs4 advisory bringup lock (issue #138). Serializes concurrent
/// `ensure_daemon` callers so two `clud` startups don't both spawn the
/// daemon.
pub(super) fn daemon_lock_path(state_dir: &Path) -> PathBuf {
    state_dir.join("daemon.lock")
}

pub(super) fn sessions_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("sessions")
}

pub(super) fn specs_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("specs")
}

pub(super) fn session_snapshot_path(state_dir: &Path, session_id: &str) -> PathBuf {
    sessions_dir(state_dir).join(format!("{session_id}.json"))
}

pub(super) fn logs_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("logs")
}

pub(super) fn daemon_events_path(state_dir: &Path) -> PathBuf {
    state_dir.join("daemon-events.jsonl")
}

pub(super) fn session_log_path(state_dir: &Path, session_id: &str) -> PathBuf {
    logs_dir(state_dir).join(format!("{session_id}.log"))
}

pub(super) fn spec_path(state_dir: &Path, session_id: &str) -> PathBuf {
    specs_dir(state_dir).join(format!("{session_id}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_dir_resolution_prefers_testable_env_home() {
        let userprofile = Some(OsString::from(r"C:\isolated-home"));
        let home = Some(OsString::from("/tmp/isolated-home"));
        let fallback = Some(PathBuf::from("/real-home"));
        let resolved = home_dir_from_parts(userprofile, home, fallback).unwrap();

        #[cfg(windows)]
        assert_eq!(resolved, PathBuf::from(r"C:\isolated-home"));

        #[cfg(not(windows))]
        assert_eq!(resolved, PathBuf::from("/tmp/isolated-home"));
    }

    #[test]
    fn home_dir_resolution_falls_back_when_env_empty() {
        let resolved = home_dir_from_parts(
            Some(OsString::new()),
            Some(OsString::new()),
            Some(PathBuf::from("/fallback-home")),
        )
        .unwrap();

        assert_eq!(resolved, PathBuf::from("/fallback-home"));
    }
}
