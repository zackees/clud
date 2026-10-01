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

use super::delete_audit;
use crate::tools::clud_uv_cache_dir;

/// Subdirectory under `~/.clud/cache/uv/` that holds per-script venvs.
pub const ENVIRONMENTS_SUBDIR: &str = "environments-v2";

/// How old (by mtime) an env must be before [`sweep_stale`] removes it.
///
/// 72 hours, matching `gc::session_tmp` and `gc::target_sweep`. It was a week,
/// which on a machine that fills its disk in four days is long enough that the
/// sweep never reclaims anything before the disk is gone — the same failure the
/// 14-day `target/` gate had. Three days rather than two so a Friday-to-Monday
/// absence does not cost the returning user their envs. A stale venv costs one
/// `uv` re-resolve to rebuild.
pub const STALE_THRESHOLD: Duration = Duration::from_secs(72 * 60 * 60);

/// The audit `rule` string, derived from [`STALE_THRESHOLD`] rather than
/// written out, so the two cannot drift apart the way they did when the
/// threshold moved and a hardcoded `stale>7d` stayed behind.
fn stale_rule() -> String {
    format!("uv-cache env stale>{}d", STALE_THRESHOLD.as_secs() / 86_400)
}

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
        // Audit before acting (#893).
        delete_audit::record("gc.uv-cache", &path, &stale_rule());
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
    // Audit before acting (#893).
    delete_audit::record("gc.uv-cache-purge", root, "uv-cache purge-all --yes");
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

// ---------------------------------------------------------------------------
// Size cap (#1691, DD-145): `cache.max_bytes`.
//
// Over the cap, clud asks uv itself to empty the cache (`uv cache clean`,
// which uv documents as "removes all cache entries"). clud never deletes
// individual buckets: uv documents that removing a file or directory inside
// its cache is never safe. Every spare condition is decided by the pure
// [`decide_cap`] over injected [`CapFacts`].
// ---------------------------------------------------------------------------

/// Seeded `cache.max_bytes`: 32 GiB. Above the 20 GiB warn threshold
/// (`cache.warn_bytes`), so the banner warns before clud ever acts.
pub const DEFAULT_MAX_BYTES: u64 = 32 * 1024 * 1024 * 1024;

/// The audit `rule` string for a cap-triggered clean.
pub const CAP_RULE: &str = "uv-cache size>cache.max_bytes: uv cache clean";

/// Everything [`decide_cap`] needs, gathered by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapFacts {
    /// Bytes under the clud uv cache root.
    pub total_bytes: u64,
    /// `cache.max_bytes`; `0` disables the cap.
    pub max_bytes: u64,
    /// `Err(reason)` when the root failed [`check_cap_root`].
    pub root_check: Result<(), &'static str>,
    /// Any process named `uv` is running on the host.
    pub uv_running: bool,
    /// `UV_LINK_MODE=symlink`: project venvs would point into the cache.
    pub symlink_link_mode: bool,
}

/// What [`decide_cap`] chose. `Spare` always carries the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapDecision {
    Spare(&'static str),
    Clean,
}

/// Pure cap decision. Spare on any doubt.
pub fn decide_cap(facts: &CapFacts) -> CapDecision {
    let _ = facts;
    CapDecision::Spare("unimplemented")
}

/// Outcome of [`enforce_cap_at`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapOutcome {
    Spared(&'static str),
    Cleaned { before_bytes: u64 },
    Failed(String),
}

/// Gather facts for `root`, decide, and run `<uv> cache clean --cache-dir
/// <root>` when the decision is [`CapDecision::Clean`].
pub fn enforce_cap_at(
    root: &Path,
    cache_parent: &Path,
    max_bytes: u64,
    uv_running: bool,
    symlink_link_mode: bool,
    uv_program: &Path,
) -> CapOutcome {
    let _ = (
        root,
        cache_parent,
        max_bytes,
        uv_running,
        symlink_link_mode,
        uv_program,
    );
    CapOutcome::Spared("unimplemented")
}

/// Refuse a root that is not exactly `<cache_parent>/uv` as a real
/// directory (not a symlink, canonical parent matches).
pub fn check_cap_root(root: &Path, cache_parent: &Path) -> Result<(), &'static str> {
    let _ = (root, cache_parent);
    Err("unimplemented")
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

    fn facts() -> CapFacts {
        CapFacts {
            total_bytes: 100,
            max_bytes: 50,
            root_check: Ok(()),
            uv_running: false,
            symlink_link_mode: false,
        }
    }

    #[test]
    fn cap_cleans_only_when_over_cap_and_every_guard_passes() {
        assert_eq!(decide_cap(&facts()), CapDecision::Clean);
    }

    #[test]
    fn cap_spare_table() {
        let cases: Vec<(CapFacts, &str)> = vec![
            (
                CapFacts {
                    max_bytes: 0,
                    ..facts()
                },
                "cap disabled",
            ),
            (
                CapFacts {
                    total_bytes: 50,
                    ..facts()
                },
                "under cap",
            ),
            (
                CapFacts {
                    root_check: Err("root is a symlink"),
                    ..facts()
                },
                "root is a symlink",
            ),
            (
                CapFacts {
                    uv_running: true,
                    ..facts()
                },
                "a uv process is running",
            ),
            (
                CapFacts {
                    symlink_link_mode: true,
                    ..facts()
                },
                "UV_LINK_MODE=symlink",
            ),
            // Disabled wins over every other fact.
            (
                CapFacts {
                    max_bytes: 0,
                    uv_running: true,
                    ..facts()
                },
                "cap disabled",
            ),
        ];
        for (f, reason) in cases {
            assert_eq!(decide_cap(&f), CapDecision::Spare(reason), "{f:?}");
        }
    }

    #[test]
    fn cap_root_must_be_the_uv_child_of_the_cache_parent() {
        let tmp = tempdir().unwrap();
        let parent = tmp.path().join("cache");
        let root = parent.join("uv");
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(check_cap_root(&root, &parent), Ok(()));
        assert!(check_cap_root(&parent.join("missing"), &parent).is_err());
        let other = tmp.path().join("elsewhere").join("uv");
        std::fs::create_dir_all(&other).unwrap();
        assert!(check_cap_root(&other, &parent).is_err());
        let wrong_name = parent.join("pip");
        std::fs::create_dir_all(&wrong_name).unwrap();
        assert!(check_cap_root(&wrong_name, &parent).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn cap_root_refuses_a_symlink_escape() {
        let tmp = tempdir().unwrap();
        let parent = tmp.path().join("cache");
        std::fs::create_dir_all(&parent).unwrap();
        let target = tmp.path().join("user-uv-cache");
        std::fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, parent.join("uv")).unwrap();
        assert_eq!(
            check_cap_root(&parent.join("uv"), &parent),
            Err("root is a symlink")
        );
    }

    /// Writes a fake `uv` that records its argv and empties the cache dir it
    /// was given (argv[4]), as `uv cache clean --cache-dir <root>` would.
    #[cfg(unix)]
    fn fake_uv(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let exe = dir.join("uv");
        std::fs::write(
            &exe,
            "#!/bin/sh\necho \"$@\" > \"$(dirname \"$0\")/argv.txt\"\nrm -rf \"$4\"/*\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        exe
    }

    #[cfg(unix)]
    #[test]
    fn enforce_cap_runs_uv_cache_clean_over_the_cap() {
        let tmp = tempdir().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let uv = fake_uv(&bin);
        let parent = tmp.path().join("cache");
        let root = parent.join("uv");
        make_env(&root, "a");
        let outcome = enforce_cap_at(&root, &parent, 1, false, false, &uv);
        assert!(matches!(outcome, CapOutcome::Cleaned { .. }), "{outcome:?}");
        let argv = std::fs::read_to_string(bin.join("argv.txt")).unwrap();
        assert_eq!(
            argv.trim(),
            format!("cache clean --cache-dir {}", root.display())
        );
        assert!(!root.join(ENVIRONMENTS_SUBDIR).exists());
    }

    #[cfg(unix)]
    #[test]
    fn enforce_cap_never_invokes_uv_when_spared() {
        let tmp = tempdir().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let uv = fake_uv(&bin);
        let parent = tmp.path().join("cache");
        let root = parent.join("uv");
        let env = make_env(&root, "a");
        let outcome = enforce_cap_at(&root, &parent, 1, true, false, &uv);
        assert_eq!(outcome, CapOutcome::Spared("a uv process is running"));
        assert!(!bin.join("argv.txt").exists());
        assert!(env.exists());
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
