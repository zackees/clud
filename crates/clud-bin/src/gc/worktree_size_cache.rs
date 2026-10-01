//! Issue #1610: launch-banner warning when `~/.clud/tmp-wt` exceeds
//! `worktrees.warn_bytes`, without walking the tree on the launch path.
//!
//! The daemon's repo-worktree probe thread calls [`refresh_cache`], which
//! runs the bounded [`check_tree_size`] walk and writes the result to
//! `~/.clud/tmp-wt-size.json` (beside `tmp-wt`). The launch path calls
//! [`launch_warning`], which reads only that small file plus the settings
//! file: no walk, no daemon round trip. A missing, corrupt, stale or
//! future-dated cache shows nothing. Warn-only: nothing is ever deleted for
//! size. See `docs/architecture/gc-and-registry.md`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::gc::worktree_root::{check_tree_size, worktree_root_for, SizeCheck};

/// File name of the cache, a sibling of `tmp-wt` under `~/.clud`.
pub const CACHE_FILE_NAME: &str = "tmp-wt-size.json";

/// A cache older than this is ignored. The GC tick is hourly by default, so
/// this tolerates several missed ticks before going quiet.
pub const MAX_CACHE_AGE_SECS: i64 = 6 * 60 * 60;

/// A timestamp further than this in the future is treated as untrustworthy.
pub const FUTURE_SKEW_SECS: i64 = 5 * 60;

/// One cached size check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachedTreeSize {
    pub checked_unix: i64,
    /// The limit the walk ran against (it stops early past it).
    pub warn_bytes: u64,
    pub check: SizeCheck,
}

/// Why the banner stays quiet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Disabled,
    NoCache,
    Stale,
    FutureTimestamp,
    Under,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerVerdict {
    Warn { bytes: u64 },
    Skip(SkipReason),
}

/// Pure decision: should the launch banner warn?
///
/// `Over(b)` is a lower bound and `Under(b)` an exact total, so both compare
/// `b` against the *current* threshold (the setting may have changed since
/// the walk). `Unknown` (entry budget exhausted) stays quiet on the banner;
/// `clud gc list` still reports it.
pub fn banner_decision(
    cached: Option<&CachedTreeSize>,
    warn_bytes: u64,
    now_unix: i64,
) -> BannerVerdict {
    // RED stub (#1610)
    let _ = (cached, warn_bytes, now_unix);
    BannerVerdict::Skip(SkipReason::NoCache)
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
}

/// The one-line banner warning.
pub fn banner_line(root: &Path, bytes: u64, warn_bytes: u64) -> String {
    // RED stub (#1610)
    let _ = (root, bytes, warn_bytes);
    String::new()
}

/// Cache path for a worktree root: `<root>/../tmp-wt-size.json`.
pub fn cache_path_for(wt_root: &Path) -> PathBuf {
    wt_root.parent().unwrap_or(wt_root).join(CACHE_FILE_NAME)
}

#[derive(Serialize, Deserialize)]
struct Wire {
    checked_unix: i64,
    warn_bytes: u64,
    state: String,
    bytes: u64,
}

/// Failure-silent read: missing or malformed means `None`.
pub fn read_cache(path: &Path) -> Option<CachedTreeSize> {
    // RED stub (#1610)
    let _ = path;
    None
}

/// Atomic write (temp file + rename) so a launch never reads a torn file.
pub fn write_cache(path: &Path, entry: &CachedTreeSize) -> std::io::Result<()> {
    // RED stub (#1610)
    let _ = (path, entry);
    Ok(())
}

/// Daemon side: run the bounded walk and record it. `warn_bytes == 0`
/// removes any old cache instead of walking.
pub fn refresh_cache(wt_root: &Path, warn_bytes: u64, now_unix: i64) -> std::io::Result<()> {
    // RED stub (#1610)
    let _ = (wt_root, warn_bytes, now_unix);
    Ok(())
}

/// Launch side, testable against any home.
pub fn launch_warning_at(home: &Path, now_unix: i64) -> Option<String> {
    // RED stub (#1610)
    let _ = (home, now_unix);
    None
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Launch side: the banner line, if any. Never fails, never walks.
pub fn launch_warning() -> Option<String> {
    let home = crate::gc::session_tmp::home_dir()?;
    launch_warning_at(&home, now_unix())
}

#[cfg(test)]
#[path = "worktree_size_cache_tests.rs"]
mod tests;
