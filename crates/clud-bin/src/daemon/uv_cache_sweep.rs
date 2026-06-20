//! Issue #423 — daemon-side daily sweep of stale uv-cache envs.
//!
//! The clud daemon's existing periodic tick (`gc_service.rs`) calls
//! [`maybe_sweep_uv_cache`] every cadence. That helper reads a sentinel
//! timestamp at `~/.clud/state/uv-cache-sweep.last`; if 24h has elapsed
//! since the last successful sweep, it invokes
//! [`crate::gc::uv_cache::sweep_stale`] and updates the sentinel.
//!
//! All errors are non-fatal — a sweep miss never crashes the daemon. The
//! worst case is one extra `cargo` resolve when uv re-materializes a
//! recently-evicted env on the next `clud tool run`.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::gc::uv_cache;

/// How often the sweep is allowed to run. 24h matches the issue spec.
pub const MIN_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Sentinel filename under the clud state dir.
const SENTINEL_FILE: &str = "uv-cache-sweep.last";

/// Production entry point — called from the daemon's periodic tick.
/// Cheap on the steady state (one file-stat + age comparison).
pub fn maybe_sweep_uv_cache() {
    let Some(sentinel) = sentinel_path() else {
        // No home dir → no state dir → nothing to do.
        return;
    };
    if let Err(e) = maybe_sweep_at(&sentinel, SystemTime::now()) {
        eprintln!("[clud] uv-cache sweep error: {e}");
    }
}

/// Testable variant of [`maybe_sweep_uv_cache`].
///
/// - Reads the sentinel at `sentinel_path` if it exists; if its content
///   is a unix timestamp newer than `now - MIN_INTERVAL`, returns Ok(None)
///   (sweep skipped — too soon).
/// - Otherwise calls [`uv_cache::sweep_stale`] with `now`, writes the
///   new sentinel, and returns Ok(Some(report)).
pub fn maybe_sweep_at(
    sentinel_path: &std::path::Path,
    now: SystemTime,
) -> std::io::Result<Option<uv_cache::SweepReport>> {
    if let Some(last) = read_sentinel(sentinel_path) {
        // duration_since Err = clock skew (sentinel in the future) →
        // treat as "we just ran" and skip; we'll recover on the next tick
        // once wall-clock catches up.
        if let Ok(age) = now.duration_since(last) {
            if age < MIN_INTERVAL {
                return Ok(None);
            }
        } else {
            return Ok(None);
        }
    }
    let report = uv_cache::sweep_stale(now, false)?;
    write_sentinel(sentinel_path, now)?;
    if report.stale_envs_removed > 0 || report.locked_envs_skipped > 0 {
        eprintln!(
            "[clud] uv-cache sweep: removed {} stale env{}, {} locked-skipped",
            report.stale_envs_removed,
            if report.stale_envs_removed == 1 {
                ""
            } else {
                "s"
            },
            report.locked_envs_skipped,
        );
    }
    Ok(Some(report))
}

fn sentinel_path() -> Option<PathBuf> {
    let home = home_dir()?;
    Some(home.join(".clud/state").join(SENTINEL_FILE))
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
    fn first_run_writes_sentinel_and_returns_report() {
        let tmp = tempdir().unwrap();
        let sentinel = tmp.path().join("state").join(SENTINEL_FILE);
        let now = SystemTime::now();
        let result = maybe_sweep_at(&sentinel, now).unwrap();
        assert!(result.is_some(), "first run must execute the sweep");
        assert!(sentinel.exists(), "sentinel file must be written");
    }

    #[test]
    fn second_run_within_interval_is_skipped() {
        let tmp = tempdir().unwrap();
        let sentinel = tmp.path().join("state").join(SENTINEL_FILE);
        let now = SystemTime::now();
        maybe_sweep_at(&sentinel, now).unwrap();
        // Second call 1h later — well under MIN_INTERVAL (24h).
        let one_hour_later = now + Duration::from_secs(3600);
        let result = maybe_sweep_at(&sentinel, one_hour_later).unwrap();
        assert!(result.is_none(), "sweep must skip when sentinel is fresh");
    }

    #[test]
    fn run_after_interval_executes_again() {
        let tmp = tempdir().unwrap();
        let sentinel = tmp.path().join("state").join(SENTINEL_FILE);
        let now = SystemTime::now();
        maybe_sweep_at(&sentinel, now).unwrap();
        // 25h later — past MIN_INTERVAL.
        let next_day = now + Duration::from_secs(25 * 60 * 60);
        let result = maybe_sweep_at(&sentinel, next_day).unwrap();
        assert!(result.is_some(), "sweep must re-execute after MIN_INTERVAL");
    }

    #[test]
    fn future_sentinel_is_treated_as_fresh_not_panic() {
        let tmp = tempdir().unwrap();
        let sentinel = tmp.path().join("state").join(SENTINEL_FILE);
        let now = SystemTime::now();
        let future = now + Duration::from_secs(60 * 60);
        // Write a sentinel in the future (clock-skew scenario).
        write_sentinel(&sentinel, future).unwrap();
        let result = maybe_sweep_at(&sentinel, now).unwrap();
        assert!(
            result.is_none(),
            "future-timestamped sentinel must skip the sweep, not panic on Err from duration_since",
        );
    }

    #[test]
    fn malformed_sentinel_triggers_fresh_sweep() {
        let tmp = tempdir().unwrap();
        let sentinel = tmp.path().join("state").join(SENTINEL_FILE);
        fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
        fs::write(&sentinel, "not-a-number").unwrap();
        let result = maybe_sweep_at(&sentinel, SystemTime::now()).unwrap();
        assert!(
            result.is_some(),
            "unparseable sentinel must fall through to sweep, not skip",
        );
    }

    #[test]
    fn min_interval_is_24_hours() {
        assert_eq!(MIN_INTERVAL, Duration::from_secs(24 * 60 * 60));
    }
}
