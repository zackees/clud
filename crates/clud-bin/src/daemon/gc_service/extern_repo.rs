use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::gc::TrackedEntry;

use super::{DEFAULT_EXTERN_REPO_STALE_AFTER_SECS, ENV_GC_EXTERN_REPO_MAX_AGE_SECS};

pub(super) fn extern_repo_stale_after() -> Duration {
    let secs = std::env::var(ENV_GC_EXTERN_REPO_MAX_AGE_SECS)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_EXTERN_REPO_STALE_AFTER_SECS);
    Duration::from_secs(secs)
}

/// An extern-repo row is purgeable once the on-disk directory has been
/// inactive (no descendant `mtime` change) for `stale_after`. Anything
/// the scanner tracks under `<repo>/.extern-repos/` is, by convention,
/// a clud-managed checkout, so this is the only gate beyond the
/// higher-level live-session check applied by `entry_is_live`.
pub(super) fn extern_repo_is_purgeable(entry: &TrackedEntry, stale_after: Duration) -> bool {
    let path = Path::new(&entry.path);
    if !path.is_dir() {
        return false;
    }
    let Some(mtime) = most_recent_mtime(path) else {
        return false;
    };
    let Ok(age) = SystemTime::now().duration_since(mtime) else {
        return false;
    };
    age >= stale_after
}

fn most_recent_mtime(path: &Path) -> Option<SystemTime> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    let mut latest = metadata.modified().ok()?;
    if metadata.is_dir() {
        let entries = std::fs::read_dir(path).ok()?;
        for entry in entries.flatten() {
            if let Some(child_mtime) = most_recent_mtime(&entry.path()) {
                if child_mtime > latest {
                    latest = child_mtime;
                }
            }
        }
    }
    Some(latest)
}
