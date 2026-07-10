//! Issue #509 — daemon-side sweep of the session temp dir (`~/.clud/tmp`).
//!
//! Mirrors `uv_cache_sweep`: the periodic tick in `gc_service.rs` calls
//! [`maybe_sweep_session_tmp`] every cadence; a sentinel at
//! `~/.clud/state/session-tmp-sweep.last` throttles the actual work to
//! [`MIN_INTERVAL`]. When due, it drops entries under `~/.clud/tmp` whose
//! mtime is older than [`crate::gc::session_tmp::STALE_THRESHOLD`] (48h).
//!
//! All errors are non-fatal — a missed sweep never crashes the daemon.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::gc::session_tmp;

/// Temp accumulates faster than the uv cache, so sweep more often than the
/// daily uv sweep — but still cheap (one stat + age compare) between runs.
pub const MIN_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

const SENTINEL_FILE: &str = "session-tmp-sweep.last";

/// Production entry point — called from the daemon's periodic tick.
pub fn maybe_sweep_session_tmp() {
    let Some(sentinel) = sentinel_path() else {
        return;
    };
    if let Err(e) = maybe_sweep_at(&sentinel, SystemTime::now()) {
        eprintln!("[clud] session-tmp sweep error: {e}");
    }
}

/// Testable variant. Skips when the sentinel is newer than
/// `now - MIN_INTERVAL`; otherwise sweeps `~/.clud/tmp` and rewrites the
/// sentinel.
pub fn maybe_sweep_at(
    sentinel_path: &std::path::Path,
    now: SystemTime,
) -> std::io::Result<Option<session_tmp::SweepReport>> {
    if let Some(last) = read_sentinel(sentinel_path) {
        match now.duration_since(last) {
            Ok(age) if age < MIN_INTERVAL => return Ok(None),
            // Clock skew (sentinel in the future) → skip, recover next tick.
            Err(_) => return Ok(None),
            _ => {}
        }
    }
    let report = session_tmp::sweep_stale(now, false)?;
    write_sentinel(sentinel_path, now)?;
    log_report(&report);
    Ok(Some(report))
}

/// Force an immediate sweep, ignoring the sentinel throttle. Used by the GC
/// tick's background thread under disk pressure, where we want to reclaim
/// now rather than wait for the next throttle window. Still rewrites the
/// sentinel so the throttled path stays consistent.
pub fn sweep_now() {
    let now = SystemTime::now();
    match session_tmp::sweep_stale(now, false) {
        Ok(report) => {
            if let Some(sentinel) = sentinel_path() {
                let _ = write_sentinel(&sentinel, now);
            }
            log_report(&report);
        }
        Err(e) => eprintln!("[clud] session-tmp sweep error: {e}"),
    }
}

fn log_report(report: &session_tmp::SweepReport) {
    if report.removed > 0 || report.skipped > 0 {
        eprintln!(
            "[clud] session-tmp sweep: removed {} entr{}, {} skipped",
            report.removed,
            if report.removed == 1 { "y" } else { "ies" },
            report.skipped,
        );
    }
}

fn sentinel_path() -> Option<PathBuf> {
    Some(home_dir()?.join(".clud/state").join(SENTINEL_FILE))
}

fn read_sentinel(path: &std::path::Path) -> Option<SystemTime> {
    let raw = fs::read_to_string(path).ok()?;
    let secs: u64 = raw.trim().parse().ok()?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
}

fn write_sentinel(path: &std::path::Path, now: SystemTime) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| std::io::Error::other("system clock before UNIX epoch"))?
        .as_secs();
    fs::write(path, secs.to_string())
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn first_run_writes_sentinel() {
        let tmp = tempdir().unwrap();
        let sentinel = tmp.path().join("state").join(SENTINEL_FILE);
        let result = maybe_sweep_at(&sentinel, SystemTime::now()).unwrap();
        assert!(result.is_some(), "first run must execute the sweep");
        assert!(sentinel.exists());
    }

    #[test]
    fn second_run_within_interval_skips() {
        let tmp = tempdir().unwrap();
        let sentinel = tmp.path().join("state").join(SENTINEL_FILE);
        let now = SystemTime::now();
        maybe_sweep_at(&sentinel, now).unwrap();
        let soon = now + Duration::from_secs(60 * 60);
        assert!(maybe_sweep_at(&sentinel, soon).unwrap().is_none());
    }

    #[test]
    fn run_after_interval_executes_again() {
        let tmp = tempdir().unwrap();
        let sentinel = tmp.path().join("state").join(SENTINEL_FILE);
        let now = SystemTime::now();
        maybe_sweep_at(&sentinel, now).unwrap();
        let later = now + Duration::from_secs(7 * 60 * 60);
        assert!(maybe_sweep_at(&sentinel, later).unwrap().is_some());
    }

    #[test]
    fn min_interval_is_6h() {
        assert_eq!(MIN_INTERVAL, Duration::from_secs(6 * 60 * 60));
    }
}
