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
    let home = dirs::home_dir()
        .ok_or_else(|| std::io::Error::other("no home directory; cannot resolve clud state dir"))?;
    Ok(home.join(".clud").join("state"))
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

pub(super) fn session_log_path(state_dir: &Path, session_id: &str) -> PathBuf {
    logs_dir(state_dir).join(format!("{session_id}.log"))
}

pub(super) fn spec_path(state_dir: &Path, session_id: &str) -> PathBuf {
    specs_dir(state_dir).join(format!("{session_id}.json"))
}
