use std::path::{Path, PathBuf};

use crate::args::Args;

use super::types::ENV_STATE_DIR;

pub(super) fn state_dir(args: &Args) -> PathBuf {
    if let Some(path) = &args.daemon_state_dir {
        return path.clone();
    }
    if let Ok(path) = std::env::var(ENV_STATE_DIR) {
        return PathBuf::from(path);
    }
    std::env::temp_dir().join("clud-daemon")
}

pub(super) fn daemon_info_path(state_dir: &Path) -> PathBuf {
    state_dir.join("daemon.json")
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
