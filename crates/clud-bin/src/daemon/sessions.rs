use std::fs;
use std::path::Path;

use super::io_helpers::read_json_file;
use super::paths::{session_snapshot_path, sessions_dir};
use super::process_utils::pid_is_alive;
use super::types::SessionSnapshot;

/// Resolve a user-provided session identifier to the canonical session ID.
/// Tries exact match, then name match, then prefix match.
pub(super) fn resolve_session_id(state_dir: &Path, input: &str) -> Result<String, String> {
    // Exact match
    let exact_path = session_snapshot_path(state_dir, input);
    if exact_path.exists() {
        return Ok(input.to_string());
    }

    // Scan all sessions for name match or prefix match
    let Ok(entries) = fs::read_dir(sessions_dir(state_dir)) else {
        return Err(format!("session '{}' not found", input));
    };

    let mut name_matches = Vec::new();
    let mut prefix_matches = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(session) = read_json_file::<SessionSnapshot>(&path) else {
            continue;
        };
        if session.name.as_deref() == Some(input) {
            name_matches.push(session.id.clone());
        }
        if session.id.starts_with(input) {
            prefix_matches.push(session.id.clone());
        }
    }

    if name_matches.len() == 1 {
        return Ok(name_matches.into_iter().next().unwrap());
    }
    if name_matches.len() > 1 {
        return Err(format!(
            "ambiguous name '{}': matches {}",
            input,
            name_matches.join(", ")
        ));
    }
    if prefix_matches.len() == 1 {
        return Ok(prefix_matches.into_iter().next().unwrap());
    }
    if prefix_matches.len() > 1 {
        return Err(format!(
            "ambiguous prefix '{}': matches {}",
            input,
            prefix_matches.join(", ")
        ));
    }

    Err(format!("session '{}' not found", input))
}

/// Return the most recently created active session.
pub(super) fn most_recent_session(state_dir: &Path) -> Option<SessionSnapshot> {
    let sessions = list_attachable_sessions(state_dir);
    sessions
        .into_iter()
        .max_by_key(|s| s.created_at.unwrap_or(0))
}

/// Return the most recently created session, *including exited ones*.
/// Used by `clud logs --last`: a session's log is valuable after it dies,
/// so we look at every snapshot on disk rather than only attachable ones.
pub(super) fn most_recent_session_any(state_dir: &Path) -> Option<SessionSnapshot> {
    let entries = fs::read_dir(sessions_dir(state_dir)).ok()?;
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .filter_map(|p| read_json_file::<SessionSnapshot>(&p).ok())
        .max_by_key(|s| s.created_at.unwrap_or(0))
}

pub(super) fn list_background_sessions(state_dir: &Path) -> Vec<SessionSnapshot> {
    let Ok(entries) = fs::read_dir(sessions_dir(state_dir)) else {
        return Vec::new();
    };
    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(session) = read_json_file::<SessionSnapshot>(&path) else {
            continue;
        };
        if !session.background {
            continue;
        }
        if !session_is_live(&session) {
            continue;
        }
        sessions.push(session);
    }
    sessions.sort_by(|left, right| left.id.cmp(&right.id));
    sessions
}

pub(super) fn list_live_session_cwds(state_dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = fs::read_dir(sessions_dir(state_dir)) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(session) = read_json_file::<SessionSnapshot>(&path) else {
            continue;
        };
        if !session_is_live(&session) {
            continue;
        }
        let Some(cwd) = session.cwd.as_deref() else {
            continue;
        };
        let Ok(canonical) = fs::canonicalize(cwd) else {
            continue;
        };
        paths.push(canonical);
    }
    paths.sort();
    paths.dedup();
    paths
}

pub(super) fn list_attachable_sessions(state_dir: &Path) -> Vec<SessionSnapshot> {
    list_background_sessions(state_dir)
        .into_iter()
        .filter(|session| session.attachable)
        .collect()
}

fn session_is_live(session: &SessionSnapshot) -> bool {
    if session.exit_code.is_some() {
        return false;
    }
    if !pid_is_alive(session.worker_pid) {
        return false;
    }
    if let Some(root_pid) = session.root_pid {
        if !pid_is_alive(root_pid) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::io_helpers::write_json_file;
    use crate::daemon::types::SessionKind;
    use tempfile::TempDir;

    fn write_snapshot(state_dir: &Path, id: &str, created_at: u64, exit_code: Option<i32>) {
        write_snapshot_with_cwd_and_pid(state_dir, id, created_at, exit_code, None, 0);
    }

    fn write_snapshot_with_cwd_and_pid(
        state_dir: &Path,
        id: &str,
        created_at: u64,
        exit_code: Option<i32>,
        cwd: Option<String>,
        worker_pid: u32,
    ) {
        let snap = SessionSnapshot {
            id: id.into(),
            kind: SessionKind::Subprocess,
            backend: None,
            launch_mode: None,
            repo_root: None,
            command: Vec::new(),
            cwd,
            name: None,
            created_at: Some(created_at),
            detachable: false,
            background: true,
            attachable: true,
            repeat_interval_secs: None,
            repeat_next_run_at: None,
            repeat_running: false,
            daemon_pid: 0,
            worker_pid,
            worker_port: 0,
            root_pid: None,
            exit_code,
            exited_at: exit_code.map(|_| created_at + 1000),
            ctrl_c: None,
        };
        write_json_file(&session_snapshot_path(state_dir, id), &snap).unwrap();
    }

    #[test]
    fn most_recent_session_any_returns_newest_including_exited() {
        // `--last` must surface the most-recently-created session even if
        // it has already exited. `most_recent_session` (the attach helper)
        // filters exited sessions; `most_recent_session_any` does not.
        let tmp = TempDir::new().unwrap();
        write_snapshot(tmp.path(), "sess-old", 100, Some(0));
        write_snapshot(tmp.path(), "sess-new", 200, Some(1));
        let found = most_recent_session_any(tmp.path()).expect("should find a session");
        assert_eq!(found.id, "sess-new");
        assert_eq!(found.exit_code, Some(1));
    }

    #[test]
    fn most_recent_session_any_none_when_dir_missing() {
        let tmp = TempDir::new().unwrap();
        let nonexistent = tmp.path().join("does-not-exist");
        assert!(most_recent_session_any(&nonexistent).is_none());
    }

    #[test]
    fn list_live_session_cwds_returns_canonical_live_cwds() {
        let tmp = TempDir::new().unwrap();
        let live_cwd = tmp.path().join("live");
        let exited_cwd = tmp.path().join("exited");
        std::fs::create_dir_all(&live_cwd).unwrap();
        std::fs::create_dir_all(&exited_cwd).unwrap();

        write_snapshot_with_cwd_and_pid(
            tmp.path(),
            "sess-live",
            1,
            None,
            Some(live_cwd.to_string_lossy().to_string()),
            std::process::id(),
        );
        write_snapshot_with_cwd_and_pid(
            tmp.path(),
            "sess-exited",
            2,
            Some(0),
            Some(exited_cwd.to_string_lossy().to_string()),
            std::process::id(),
        );
        write_snapshot_with_cwd_and_pid(
            tmp.path(),
            "sess-dead-worker",
            3,
            None,
            Some(exited_cwd.to_string_lossy().to_string()),
            u32::MAX,
        );

        let paths = list_live_session_cwds(tmp.path());
        assert_eq!(paths, vec![std::fs::canonicalize(live_cwd).unwrap()]);
    }
}
