//! Strong liveness signals for the session-temp size cap (#2397, DD-194).
//!
//! An mtime says only that a unit was quiet, not that nobody is using it: a
//! session parked on a long CI wait or overnight writes nothing for hours and
//! still expects its scratch. So before [`super::session_tmp_cap`] evicts an
//! idle unit it asks this module whether anything points at it:
//!
//! - a running process whose cwd, or `TMPDIR`/`TMP`/`TEMP`/`CLAUDE_CODE_TMPDIR`
//!   value, lies inside the unit; or
//! - a unit path component equal to a session id the daemon still tracks (a
//!   non-exited snapshot under `<state>/sessions`, a non-terminated record
//!   under `<state>/api-sessions`).
//!
//! Fail safe: when a source cannot be consulted (the process scan cannot even
//! read this process's own cwd, or the session records are unreadable) every
//! unit counts as live, so the cap simply stays exceeded.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Env vars that name a process's scratch directory.
const SCRATCH_ENV: &[&str] = &["TMPDIR", "TMP", "TEMP", "CLAUDE_CODE_TMPDIR"];

/// What is known to be in use right now. `None` in a field means that source
/// could not be consulted, which makes every unit live.
#[derive(Debug, Clone, Default)]
pub struct LiveSignals {
    paths: Option<Vec<PathBuf>>,
    session_ids: Option<HashSet<String>>,
}

impl LiveSignals {
    /// Signals from explicit parts (production scan results, or a test).
    pub fn new(paths: Option<Vec<PathBuf>>, session_ids: Option<HashSet<String>>) -> Self {
        Self { paths, session_ids }
    }

    /// Both sources consulted and nothing found.
    pub fn nothing_live() -> Self {
        Self::new(Some(Vec::new()), Some(HashSet::new()))
    }

    /// Scan the host processes and the daemon's session records.
    pub fn collect() -> Self {
        let ids = crate::daemon::default_state_dir()
            .ok()
            .and_then(|state| tracked_session_ids(&state));
        Self::new(process_paths(), ids)
    }

    /// Is `unit` (a path under `root`) pointed at by a live process or a
    /// tracked session? An unconsulted source answers `true`.
    pub fn is_live(&self, root: &Path, unit: &Path) -> bool {
        let (Some(paths), Some(ids)) = (&self.paths, &self.session_ids) else {
            return true;
        };
        let canonical = fs::canonicalize(unit).unwrap_or_else(|_| unit.to_path_buf());
        if paths
            .iter()
            .any(|p| p.starts_with(unit) || p.starts_with(&canonical))
        {
            return true;
        }
        let rel = unit.strip_prefix(root).unwrap_or(unit);
        rel.components()
            .any(|c| ids.contains(c.as_os_str().to_string_lossy().as_ref()))
    }
}

fn env_value<'a>(environ: &'a [std::ffi::OsString], key: &str) -> Option<&'a str> {
    environ.iter().find_map(|entry| {
        entry
            .to_str()?
            .strip_prefix(key)?
            .strip_prefix('=')
            .filter(|v| !v.is_empty())
    })
}

/// Every process cwd and scratch-env path on the host; `None` when cwd reading
/// does not work here (our own cwd is unreadable), so absence proves nothing.
fn process_paths() -> Option<Vec<PathBuf>> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cwd(UpdateKind::Always)
            .with_environ(UpdateKind::Always),
    );
    let own = system.process(Pid::from_u32(std::process::id()))?;
    if own.cwd().is_none_or(|c| c.as_os_str().is_empty()) {
        return None;
    }
    let mut paths = Vec::new();
    for process in system.processes().values() {
        let cwd = process.cwd().filter(|c| !c.as_os_str().is_empty());
        paths.extend(cwd.map(Path::to_path_buf));
        for key in SCRATCH_ENV {
            paths.extend(env_value(process.environ(), key).map(PathBuf::from));
        }
    }
    paths.sort();
    paths.dedup();
    Some(paths)
}

/// Ids of sessions the daemon still tracks, from its state directory.
/// `None` when a records directory exists but cannot be read.
pub fn tracked_session_ids(state_dir: &Path) -> Option<HashSet<String>> {
    let mut ids = HashSet::new();
    for (sub, is_live) in [
        (
            "sessions",
            snapshot_is_live as fn(&serde_json::Value) -> bool,
        ),
        ("api-sessions", api_record_is_live),
    ] {
        let entries = match fs::read_dir(state_dir.join(sub)) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        };
        for path in entries.flatten().map(|e| e.path()) {
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            // An unreadable or unparsable record is doubt: fail safe.
            let doc = fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())?;
            if is_live(&doc) {
                collect_ids(&doc, &mut ids);
            }
        }
    }
    Some(ids)
}

fn collect_ids(doc: &serde_json::Value, ids: &mut HashSet<String>) {
    for key in ["id", "provider_session_id", "session_id"] {
        if let Some(id) = doc.get(key).and_then(|v| v.as_str()) {
            ids.insert(id.to_string());
        }
    }
}

/// A worker snapshot is tracked until it records an exit code.
fn snapshot_is_live(doc: &serde_json::Value) -> bool {
    doc.get("exit_code").is_none_or(serde_json::Value::is_null)
}

/// An API session is tracked until it is `terminated`.
fn api_record_is_live(doc: &serde_json::Value) -> bool {
    doc.get("state").and_then(|s| s.as_str()) != Some("terminated")
}

#[cfg(test)]
#[path = "session_tmp_live_tests.rs"]
mod tests;
