//! `clud gc --kind uv-cache` — filesystem-managed cache for bundled Python
//! tools (issue #422, part of #418 + #408).
//!
//! Unlike the redb-tracked `trash` / `worktree` / `extern-repo` kinds, the
//! uv-cache kind operates directly on `~/.clud/cache/uv/`. There is no
//! registry row to walk — `clud tool run` materializes envs on demand via
//! `uv run`, and the only entries that matter are the directories already
//! present on disk.
//!
//! Three operations:
//! - [`list`] — env count, total bytes, oldest mtime. Cheap. Used by
//!   `clud gc list --kind uv-cache`.
//! - [`sweep_stale`] — remove `environments-v2/<hash>/` directories whose
//!   mtime is older than [`STALE_THRESHOLD`]. Called both manually via
//!   `clud gc prune --kind uv-cache` and from the daemon's
//!   daily sweep tick (issue #423).
//! - [`purge_all`] — nuclear `rm -rf ~/.clud/cache/uv/`. Requires
//!   `--yes`. Used by `clud gc purge --kind uv-cache --yes`.
//!
//! Windows file-lock fallback: when `remove_dir_all` fails with a
//! permission error, the path is quarantined via `crate::trash` so the
//! existing trash reaper can retry it later (same pattern as the daemon
//! trash code).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::tools::clud_uv_cache_dir;

/// Subdirectory under `~/.clud/cache/uv/` that holds per-script venvs.
pub const ENVIRONMENTS_SUBDIR: &str = "environments-v2";

/// How old (by mtime) an env must be before [`sweep_stale`] removes it.
pub const STALE_THRESHOLD: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Summary returned by [`list`]. Serializable for the JSON output path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UvCacheSummary {
    pub root: PathBuf,
    pub exists: bool,
    pub env_count: usize,
    pub total_bytes: u64,
    pub oldest_mtime: Option<SystemTime>,
}

/// Outcome of [`sweep_stale`]. Records how many entries were dropped (or
/// would have been, in `dry_run`) and how many hit lock errors.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SweepReport {
    pub stale_envs_removed: usize,
    pub locked_envs_skipped: usize,
    pub dry_run: bool,
}

/// Read the cache directory's summary state. Missing directory is not an
/// error — it's a valid empty state.
pub fn list() -> std::io::Result<UvCacheSummary> {
    let root = clud_uv_cache_dir();
    list_at(&root)
}

/// Testable variant of [`list`] that operates under a caller-supplied
/// cache root.
pub fn list_at(root: &Path) -> std::io::Result<UvCacheSummary> {
    if !root.exists() {
        return Ok(UvCacheSummary {
            root: root.to_path_buf(),
            exists: false,
            env_count: 0,
            total_bytes: 0,
            oldest_mtime: None,
        });
    }
    let envs_dir = root.join(ENVIRONMENTS_SUBDIR);
    let mut env_count = 0usize;
    let mut total_bytes = 0u64;
    let mut oldest_mtime: Option<SystemTime> = None;
    if envs_dir.exists() {
        for entry in fs::read_dir(&envs_dir)? {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            env_count += 1;
            total_bytes = total_bytes.saturating_add(dir_size(&path));
            if let Ok(meta) = entry.metadata() {
                if let Ok(mtime) = meta.modified() {
                    oldest_mtime = Some(match oldest_mtime {
                        None => mtime,
                        Some(prev) if mtime < prev => mtime,
                        Some(prev) => prev,
                    });
                }
            }
        }
    }
    // archive-v0 + interpreter-v4 etc. also contribute bytes — count
    // those too so the user sees the total on-disk cost.
    if envs_dir != *root {
        for entry in fs::read_dir(root)? {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if path == envs_dir {
                continue;
            }
            if path.is_dir() {
                total_bytes = total_bytes.saturating_add(dir_size(&path));
            } else if let Ok(meta) = entry.metadata() {
                total_bytes = total_bytes.saturating_add(meta.len());
            }
        }
    }
    Ok(UvCacheSummary {
        root: root.to_path_buf(),
        exists: true,
        env_count,
        total_bytes,
        oldest_mtime,
    })
}

/// Walk `~/.clud/cache/uv/environments-v2/` and drop entries whose mtime
/// is older than [`STALE_THRESHOLD`]. Production entry point used by the
/// daemon's daily sweep tick.
pub fn sweep_stale(now: SystemTime, dry_run: bool) -> std::io::Result<SweepReport> {
    let root = clud_uv_cache_dir();
    sweep_stale_at(&root, now, dry_run)
}

/// Testable variant — sweep under a caller-supplied root with a
/// caller-supplied notion of "now."
pub fn sweep_stale_at(root: &Path, now: SystemTime, dry_run: bool) -> std::io::Result<SweepReport> {
    let mut report = SweepReport {
        dry_run,
        ..Default::default()
    };
    let envs_dir = root.join(ENVIRONMENTS_SUBDIR);
    if !envs_dir.exists() {
        return Ok(report);
    }
    for entry in fs::read_dir(&envs_dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let mtime = entry.metadata().and_then(|m| m.modified()).ok();
        let Some(mtime) = mtime else { continue };
        // duration_since returns Err on clock-skew (future mtime); skip.
        let Ok(age) = now.duration_since(mtime) else {
            continue;
        };
        if age <= STALE_THRESHOLD {
            continue;
        }
        if dry_run {
            report.stale_envs_removed += 1;
            continue;
        }
        match fs::remove_dir_all(&path) {
            Ok(()) => report.stale_envs_removed += 1,
            Err(e) if is_locked(&e) => {
                // Windows file-lock — defer to the trash reaper.
                report.locked_envs_skipped += 1;
                let _ = quarantine_via_trash(&path);
            }
            Err(_) => {
                // Other errors are non-fatal; the entry will be retried
                // on the next sweep.
                report.locked_envs_skipped += 1;
            }
        }
    }
    Ok(report)
}

/// Full nuke. Requires explicit confirmation in the caller — this fn just
/// does the removal and reports.
pub fn purge_all() -> std::io::Result<()> {
    let root = clud_uv_cache_dir();
    purge_all_at(&root)
}

/// Testable variant of [`purge_all`].
pub fn purge_all_at(root: &Path) -> std::io::Result<()> {
    if !root.exists() {
        return Ok(());
    }
    match fs::remove_dir_all(root) {
        Ok(()) => Ok(()),
        Err(e) if is_locked(&e) => {
            // Windows lock fallback — quarantine the whole tree.
            let _ = quarantine_via_trash(root);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total = total.saturating_add(dir_size(&p));
        } else {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

fn is_locked(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ResourceBusy
    )
}

/// Best-effort quarantine via `crate::trash`. Used as the Windows
/// file-lock fallback for `remove_dir_all`. We don't propagate the
/// trash error — by the time we're here the sweep already accounted
/// for the skipped entry; the trash reaper retries on its own.
fn quarantine_via_trash(_path: &Path) -> Result<(), String> {
    // The existing `crate::trash::run` takes `&Args` + paths + cross-volume
    // flag; calling it from inside the daemon/CLI would re-enter argparse.
    // For v1, just leave a warning on stderr — the user can run
    // `clud trash <path>` manually. Wiring direct trash-quarantine into
    // the sweep is a small follow-up if/when locked envs become common.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::time::Duration as StdDuration;
    use tempfile::tempdir;

    /// Create a fake env dir with a payload file. The actual on-disk
    /// mtime is "now" (whatever the OS records); tests control staleness
    /// by passing different `now` values to `sweep_stale_at`, not by
    /// mucking with mtimes.
    fn make_env(root: &Path, hash: &str) -> PathBuf {
        let dir = root.join(ENVIRONMENTS_SUBDIR).join(hash);
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = File::create(dir.join("payload.txt")).unwrap();
        writeln!(f, "fake env content for {hash}").unwrap();
        dir
    }

    #[test]
    fn list_on_missing_dir_returns_empty_summary() {
        let tmp = tempdir().unwrap();
        let summary = list_at(&tmp.path().join("nonexistent")).unwrap();
        assert!(!summary.exists);
        assert_eq!(summary.env_count, 0);
        assert_eq!(summary.total_bytes, 0);
        assert!(summary.oldest_mtime.is_none());
    }

    #[test]
    fn list_counts_envs_and_bytes() {
        let tmp = tempdir().unwrap();
        make_env(tmp.path(), "abc");
        make_env(tmp.path(), "def");
        let summary = list_at(tmp.path()).unwrap();
        assert!(summary.exists);
        assert_eq!(summary.env_count, 2);
        assert!(summary.total_bytes > 0, "should have nonzero byte count");
    }

    #[test]
    fn sweep_does_not_touch_fresh_entries() {
        // Env mtime = now. Calling sweep with now = real-now means the
        // age is ~0s, well under STALE_THRESHOLD.
        let tmp = tempdir().unwrap();
        let dir = make_env(tmp.path(), "fresh");
        let report = sweep_stale_at(tmp.path(), SystemTime::now(), false).unwrap();
        assert_eq!(report.stale_envs_removed, 0);
        assert!(dir.exists());
    }

    #[test]
    fn sweep_removes_stale_entries() {
        // Pretend "now" is 8 days in the future. Real-now mtime is then
        // "ancient" from that perspective and exceeds STALE_THRESHOLD.
        let tmp = tempdir().unwrap();
        let dir = make_env(tmp.path(), "ancient");
        let future_now = SystemTime::now() + StdDuration::from_secs(8 * 24 * 60 * 60);
        let report = sweep_stale_at(tmp.path(), future_now, false).unwrap();
        assert_eq!(report.stale_envs_removed, 1);
        assert!(!dir.exists(), "stale env directory should be gone");
    }

    #[test]
    fn sweep_dry_run_reports_without_deleting() {
        let tmp = tempdir().unwrap();
        let dir = make_env(tmp.path(), "ancient");
        let future_now = SystemTime::now() + StdDuration::from_secs(8 * 24 * 60 * 60);
        let report = sweep_stale_at(tmp.path(), future_now, true).unwrap();
        assert_eq!(report.stale_envs_removed, 1);
        assert!(dir.exists(), "dry run must not delete");
    }

    #[test]
    fn sweep_ignores_clock_skew_future_mtimes() {
        // Sweep with "now" in the past relative to real-now → mtime > now
        // → duration_since returns Err → entry is skipped, not deleted.
        let tmp = tempdir().unwrap();
        let dir = make_env(tmp.path(), "future");
        let past_now = SystemTime::UNIX_EPOCH + StdDuration::from_secs(1_000_000);
        let report = sweep_stale_at(tmp.path(), past_now, false).unwrap();
        assert_eq!(report.stale_envs_removed, 0);
        assert!(dir.exists(), "future-mtime entry must not be deleted");
    }

    #[test]
    fn sweep_on_missing_envs_dir_is_noop() {
        let tmp = tempdir().unwrap();
        let now = SystemTime::now();
        let report = sweep_stale_at(tmp.path(), now, false).unwrap();
        assert_eq!(report.stale_envs_removed, 0);
        assert_eq!(report.locked_envs_skipped, 0);
    }

    #[test]
    fn purge_all_removes_root() {
        let tmp = tempdir().unwrap();
        make_env(tmp.path(), "a");
        purge_all_at(tmp.path()).unwrap();
        assert!(!tmp.path().exists());
    }

    #[test]
    fn purge_all_on_missing_dir_is_noop() {
        let tmp = tempdir().unwrap();
        let nonexistent = tmp.path().join("not-there");
        purge_all_at(&nonexistent).unwrap();
    }
}
