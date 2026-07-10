//! Session temp directory (`~/.clud/tmp`) — issue #509.
//!
//! While a clud session is active we point the backend agent's temp dir
//! (`TMPDIR` on Unix, `TMP`+`TEMP` on Windows) at a clud-owned location so
//! the scatter of agent/tooling temp files lands somewhere the daemon can
//! reclaim, instead of the OS temp dir where nothing ages it out.
//!
//! Two halves:
//! - [`ensure_dir`] — resolve `~/.clud/tmp`, create it, hand the path back
//!   to the env builders in `runner.rs` / `daemon/io_helpers.rs`.
//! - [`sweep_stale_at`] — drop top-level entries whose mtime is older than
//!   [`STALE_THRESHOLD`] (48h). Driven from the daemon's periodic tick via
//!   `daemon/session_tmp_sweep.rs`.
//!
//! Like `gc::uv_cache`, this operates directly on the filesystem — there is
//! no redb registry row. All errors are non-fatal: a failed sweep never
//! crashes the daemon, and a failed `ensure_dir` just falls back to the OS
//! temp dir (the env vars are simply not overridden).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How old (by mtime) a top-level entry must be before [`sweep_stale_at`]
/// removes it. 48h matches the worktree GC policy value but is a *separate*
/// constant — session-temp lifetime and worktree staleness are independent
/// policies that only happen to share a number today (issue #509).
pub const STALE_THRESHOLD: Duration = Duration::from_secs(48 * 60 * 60);

/// Outcome of [`sweep_stale_at`]. `removed` counts files+dirs dropped (or,
/// in `dry_run`, that would have been); `skipped` counts lock/permission
/// failures that a later sweep will retry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SweepReport {
    pub removed: usize,
    pub skipped: usize,
    pub dry_run: bool,
}

/// Opt-out env var. Set to `0`/`false`/`no`/`off` to keep the OS temp dir.
pub const DISABLE_ENV: &str = "CLUD_SESSION_TMP";

/// Temp env vars we override at session launch. `TMPDIR` is the Unix
/// convention; `TMP`/`TEMP` are what Windows `GetTempPath` consults (and
/// some cross-platform tooling reads on Unix too). Setting all three is the
/// most robust. Also the set of keys to strip before re-adding, so we never
/// pass a stale inherited value alongside the override.
pub const OVERRIDDEN_KEYS: &[&str] = &["TMPDIR", "TMP", "TEMP"];

/// Resolve `~/.clud/tmp` without creating it. `None` when no home dir can
/// be determined (headless/misconfigured env) — the caller then leaves the
/// OS temp dir in place.
pub fn session_tmp_dir() -> Option<PathBuf> {
    Some(home_dir()?.join(".clud").join("tmp"))
}

/// Whether the redirect is disabled via [`DISABLE_ENV`]. Default: enabled.
pub fn is_disabled() -> bool {
    match std::env::var(DISABLE_ENV) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => false,
    }
}

/// The `(key, value)` temp-env overrides to layer onto a child environment,
/// or an empty vec when the redirect is disabled or the dir can't be created
/// (in which case the child keeps the OS temp dir). Creates `~/.clud/tmp` as
/// a side effect on the success path.
pub fn env_overrides() -> Vec<(String, String)> {
    if is_disabled() {
        return Vec::new();
    }
    let Some(dir) = ensure_dir() else {
        return Vec::new();
    };
    build_overrides(&dir)
}

fn build_overrides(dir: &Path) -> Vec<(String, String)> {
    let value = dir.to_string_lossy().into_owned();
    OVERRIDDEN_KEYS
        .iter()
        .map(|key| ((*key).to_string(), value.clone()))
        .collect()
}

/// Resolve `~/.clud/tmp` and create it. Returns the path on success so the
/// env builders can point `TMPDIR`/`TMP`/`TEMP` at it. Any failure (no home,
/// unwritable volume) yields `None` and the caller keeps the OS temp dir.
pub fn ensure_dir() -> Option<PathBuf> {
    let dir = session_tmp_dir()?;
    match fs::create_dir_all(&dir) {
        Ok(()) => Some(dir),
        Err(_) => None,
    }
}

/// Production sweep entry point — called from the daemon's periodic tick.
pub fn sweep_stale(now: SystemTime, dry_run: bool) -> std::io::Result<SweepReport> {
    let Some(root) = session_tmp_dir() else {
        return Ok(SweepReport {
            dry_run,
            ..Default::default()
        });
    };
    sweep_stale_at(&root, now, dry_run)
}

/// Testable variant — sweep under a caller-supplied root with a
/// caller-supplied notion of "now". Missing directory is a valid empty
/// state, not an error.
pub fn sweep_stale_at(root: &Path, now: SystemTime, dry_run: bool) -> std::io::Result<SweepReport> {
    let mut report = SweepReport {
        dry_run,
        ..Default::default()
    };
    if !root.exists() {
        return Ok(report);
    }
    for entry in fs::read_dir(root)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(mtime) = meta.modified() else { continue };
        // duration_since returns Err on clock skew (future mtime) — skip it;
        // we'll reconsider once wall-clock catches up.
        let Ok(age) = now.duration_since(mtime) else {
            continue;
        };
        if age <= STALE_THRESHOLD {
            continue;
        }
        if dry_run {
            report.removed += 1;
            continue;
        }
        let result = if meta.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        match result {
            Ok(()) => report.removed += 1,
            // Non-fatal (Windows lock, races) — retried on the next sweep.
            Err(_) => report.skipped += 1,
        }
    }
    Ok(report)
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(path) = std::env::var_os("USERPROFILE") {
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }
    if let Some(path) = std::env::var_os("HOME") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::time::Duration as StdDuration;
    use tempfile::tempdir;

    fn make_file(root: &Path, name: &str) -> PathBuf {
        let path = root.join(name);
        let mut f = File::create(&path).unwrap();
        writeln!(f, "temp payload {name}").unwrap();
        path
    }

    fn make_subdir(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        make_file(&dir, "inner.txt");
        dir
    }

    #[test]
    fn stale_threshold_is_48h() {
        assert_eq!(STALE_THRESHOLD, Duration::from_secs(48 * 60 * 60));
    }

    #[test]
    fn sweep_on_missing_dir_is_noop() {
        let tmp = tempdir().unwrap();
        let report = sweep_stale_at(&tmp.path().join("nope"), SystemTime::now(), false).unwrap();
        assert_eq!(report.removed, 0);
        assert_eq!(report.skipped, 0);
    }

    #[test]
    fn sweep_leaves_fresh_entries() {
        let tmp = tempdir().unwrap();
        let f = make_file(tmp.path(), "fresh.txt");
        let d = make_subdir(tmp.path(), "fresh-dir");
        let report = sweep_stale_at(tmp.path(), SystemTime::now(), false).unwrap();
        assert_eq!(report.removed, 0);
        assert!(f.exists());
        assert!(d.exists());
    }

    #[test]
    fn sweep_removes_stale_files_and_dirs() {
        let tmp = tempdir().unwrap();
        let f = make_file(tmp.path(), "old.txt");
        let d = make_subdir(tmp.path(), "old-dir");
        // Pretend "now" is 49h in the future so the just-created entries
        // read as older than the 48h threshold.
        let future_now = SystemTime::now() + StdDuration::from_secs(49 * 60 * 60);
        let report = sweep_stale_at(tmp.path(), future_now, false).unwrap();
        assert_eq!(report.removed, 2);
        assert!(!f.exists());
        assert!(!d.exists());
    }

    #[test]
    fn sweep_dry_run_reports_without_deleting() {
        let tmp = tempdir().unwrap();
        let f = make_file(tmp.path(), "old.txt");
        let future_now = SystemTime::now() + StdDuration::from_secs(49 * 60 * 60);
        let report = sweep_stale_at(tmp.path(), future_now, true).unwrap();
        assert_eq!(report.removed, 1);
        assert!(f.exists(), "dry run must not delete");
    }

    #[test]
    fn sweep_ignores_future_mtimes() {
        let tmp = tempdir().unwrap();
        let f = make_file(tmp.path(), "future.txt");
        // "now" far in the past → entry mtime is in the future → skipped.
        let past_now = SystemTime::UNIX_EPOCH + StdDuration::from_secs(1_000_000);
        let report = sweep_stale_at(tmp.path(), past_now, false).unwrap();
        assert_eq!(report.removed, 0);
        assert!(f.exists());
    }

    #[test]
    fn build_overrides_sets_all_three_keys_to_dir() {
        let dir = Path::new("/some/clud/tmp");
        let overrides = build_overrides(dir);
        let keys: Vec<&str> = overrides.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["TMPDIR", "TMP", "TEMP"]);
        for (_, v) in &overrides {
            assert_eq!(v, &dir.to_string_lossy());
        }
    }

    #[test]
    fn ensure_dir_creates_under_home() {
        let tmp = tempdir().unwrap();
        let _guard = HomeGuard::set(tmp.path());
        let dir = ensure_dir().expect("ensure_dir should succeed with a valid HOME");
        assert!(dir.exists());
        assert!(dir.ends_with("tmp"));
        assert!(dir.starts_with(tmp.path()));
    }

    /// RAII guard swapping HOME/USERPROFILE for the resolution test. `std::env`
    /// is process-global so serialize via a mutex.
    struct HomeGuard {
        prior_home: Option<String>,
        prior_userprofile: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl HomeGuard {
        fn set(dir: &Path) -> Self {
            static M: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
            let lock = M
                .get_or_init(|| std::sync::Mutex::new(()))
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let prior_home = std::env::var("HOME").ok();
            let prior_userprofile = std::env::var("USERPROFILE").ok();
            std::env::set_var("HOME", dir);
            std::env::set_var("USERPROFILE", dir);
            Self {
                prior_home,
                prior_userprofile,
                _lock: lock,
            }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.prior_home.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match self.prior_userprofile.take() {
                Some(v) => std::env::set_var("USERPROFILE", v),
                None => std::env::remove_var("USERPROFILE"),
            }
        }
    }
}
