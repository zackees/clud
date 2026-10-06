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
//!
//! Issue #1327 reuses the same mechanism for the session temp root
//! `~/.clud/tmp` behind `tmp.warn_bytes`: the daemon's maintenance sweep
//! thread calls [`refresh_tmp_cache`] (cache file `~/.clud/tmp-size.json`,
//! a sibling of `tmp` so the 72 h sweep never sees it) and the launch path
//! calls [`tmp_launch_warning`]. Also warn-only: the age-based sweep remains
//! the only thing that deletes session temp (DD-141).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::gc::worktree_root::{check_tree_size, worktree_root_for, SizeCheck};

/// File name of the cache, a sibling of `tmp-wt` under `~/.clud`.
pub const CACHE_FILE_NAME: &str = "tmp-wt-size.json";

/// File name of the `~/.clud/tmp` size cache, a sibling of `tmp` (#1327).
pub const TMP_CACHE_FILE_NAME: &str = "tmp-size.json";

/// Default for `tmp.warn_bytes`: 20 GiB. `0` disables the warning. Four
/// times the per-session report threshold (`SIZE_REPORT_THRESHOLD`), small
/// enough to speak up long before the 100+ GiB trees of #1327 and #1672.
pub const DEFAULT_TMP_WARN_BYTES: u64 = 20 * 1024 * 1024 * 1024;

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
    if warn_bytes == 0 {
        return BannerVerdict::Skip(SkipReason::Disabled);
    }
    let Some(cached) = cached else {
        return BannerVerdict::Skip(SkipReason::NoCache);
    };
    let age = now_unix.saturating_sub(cached.checked_unix);
    if age < -FUTURE_SKEW_SECS {
        return BannerVerdict::Skip(SkipReason::FutureTimestamp);
    }
    if age > MAX_CACHE_AGE_SECS {
        return BannerVerdict::Skip(SkipReason::Stale);
    }
    match cached.check {
        SizeCheck::Unknown => BannerVerdict::Skip(SkipReason::Unknown),
        SizeCheck::Over(bytes) | SizeCheck::Under(bytes) if bytes > warn_bytes => {
            BannerVerdict::Warn { bytes }
        }
        SizeCheck::Over(_) | SizeCheck::Under(_) => BannerVerdict::Skip(SkipReason::Under),
    }
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
}

/// The one-line banner warning.
pub fn banner_line(root: &Path, bytes: u64, warn_bytes: u64) -> String {
    banner_line_for("worktrees.warn_bytes", root, bytes, warn_bytes)
}

/// The one-line banner warning for `~/.clud/tmp` (#1327).
pub fn tmp_banner_line(root: &Path, bytes: u64, warn_bytes: u64) -> String {
    banner_line_for("tmp.warn_bytes", root, bytes, warn_bytes)
}

fn banner_line_for(setting: &str, root: &Path, bytes: u64, warn_bytes: u64) -> String {
    format!(
        "[clud] warning: {} holds at least {}, over {setting} ({}); \
         review `clud gc list`, or raise {setting} in ~/.clud/settings.json (0 disables)",
        root.display(),
        gib(bytes),
        gib(warn_bytes)
    )
}

/// The `clud gc list` warning line for `~/.clud/tmp` (#1327), or `None`
/// when there is nothing to say. `warn_bytes == 0` disables it.
pub fn tmp_list_warning(root: &Path, warn_bytes: u64, check: SizeCheck) -> Option<String> {
    if warn_bytes == 0 {
        return None;
    }
    match check {
        SizeCheck::Under(_) => None,
        SizeCheck::Over(bytes) => Some(format!(
            "warning: {} holds at least {bytes} bytes, over tmp.warn_bytes ({warn_bytes}); \
             session temp is never deleted for size (only the 72 h age sweep reclaims it) \
             — remove what you no longer need",
            root.display()
        )),
        SizeCheck::Unknown => Some(format!(
            "warning: {} is too large to size within budget; it may exceed \
             tmp.warn_bytes ({warn_bytes})",
            root.display()
        )),
    }
}

/// Cache path for the session temp root: `<home>/.clud/tmp-size.json`.
pub fn tmp_cache_path_for(home: &Path) -> PathBuf {
    let tmp = crate::gc::session_tmp::session_tmp_dir_for(home);
    tmp.parent().unwrap_or(&tmp).join(TMP_CACHE_FILE_NAME)
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
    let text = fs::read_to_string(path).ok()?;
    let wire: Wire = serde_json::from_str(&text).ok()?;
    let check = match wire.state.as_str() {
        "over" => SizeCheck::Over(wire.bytes),
        "under" => SizeCheck::Under(wire.bytes),
        "unknown" => SizeCheck::Unknown,
        _ => return None,
    };
    Some(CachedTreeSize {
        checked_unix: wire.checked_unix,
        warn_bytes: wire.warn_bytes,
        check,
    })
}

/// Atomic write (temp file + rename) so a launch never reads a torn file.
pub fn write_cache(path: &Path, entry: &CachedTreeSize) -> std::io::Result<()> {
    let (state, bytes) = match entry.check {
        SizeCheck::Over(b) => ("over", b),
        SizeCheck::Under(b) => ("under", b),
        SizeCheck::Unknown => ("unknown", 0),
    };
    let wire = Wire {
        checked_unix: entry.checked_unix,
        warn_bytes: entry.warn_bytes,
        state: state.to_string(),
        bytes,
    };
    let text = serde_json::to_string(&wire).map_err(std::io::Error::other)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Daemon side: run the bounded walk and record it. `warn_bytes == 0`
/// removes any old cache instead of walking.
pub fn refresh_cache(wt_root: &Path, warn_bytes: u64, now_unix: i64) -> std::io::Result<()> {
    refresh_cache_at(&cache_path_for(wt_root), wt_root, warn_bytes, now_unix)
}

/// Daemon side for `~/.clud/tmp` (#1327). Read-only walk; deletes nothing
/// but a stale cache file when the warning is disabled.
pub fn refresh_tmp_cache(home: &Path, warn_bytes: u64, now_unix: i64) -> std::io::Result<()> {
    let root = crate::gc::session_tmp::session_tmp_dir_for(home);
    refresh_cache_at(&tmp_cache_path_for(home), &root, warn_bytes, now_unix)
}

fn refresh_cache_at(
    path: &Path,
    root: &Path,
    warn_bytes: u64,
    now_unix: i64,
) -> std::io::Result<()> {
    if warn_bytes == 0 {
        return match fs::remove_file(path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        };
    }
    let check = check_tree_size(
        root,
        warn_bytes,
        crate::gc::worktree_root::SIZE_SCAN_ENTRY_BUDGET,
    );
    write_cache(
        path,
        &CachedTreeSize {
            checked_unix: now_unix,
            warn_bytes,
            check,
        },
    )
}

/// Launch side, testable against any home.
pub fn launch_warning_at(home: &Path, now_unix: i64) -> Option<String> {
    let warn_bytes = crate::clud_settings::peek_worktrees_warn_bytes_at(home);
    if warn_bytes == 0 {
        return None;
    }
    let root = worktree_root_for(home);
    let cached = read_cache(&cache_path_for(&root));
    match banner_decision(cached.as_ref(), warn_bytes, now_unix) {
        BannerVerdict::Warn { bytes } => Some(banner_line(&root, bytes, warn_bytes)),
        BannerVerdict::Skip(_) => None,
    }
}

/// Launch side for `~/.clud/tmp` (#1327), testable against any home.
pub fn tmp_launch_warning_at(home: &Path, now_unix: i64) -> Option<String> {
    let warn_bytes = crate::clud_settings::peek_tmp_warn_bytes_at(home);
    if warn_bytes == 0 {
        return None;
    }
    let cached = read_cache(&tmp_cache_path_for(home));
    match banner_decision(cached.as_ref(), warn_bytes, now_unix) {
        BannerVerdict::Warn { bytes } => Some(tmp_banner_line(
            &crate::gc::session_tmp::session_tmp_dir_for(home),
            bytes,
            warn_bytes,
        )),
        BannerVerdict::Skip(_) => None,
    }
}

/// Launch side for `~/.clud/tmp`: never fails, never walks.
pub fn tmp_launch_warning() -> Option<String> {
    let home = crate::home::user_home()?;
    tmp_launch_warning_at(&home, now_unix())
}

// ---- #1691: the same warn-only cached size check for `~/.clud/cache` ----

/// File name of the `~/.clud/cache` size cache, a sibling of `cache` so uv
/// (which owns everything inside `cache/uv`) never sees it.
pub const CLUD_CACHE_SIZE_FILE_NAME: &str = "cache-size.json";

/// Default for `cache.warn_bytes`: 20 GiB, same as `tmp.warn_bytes`. The
/// #1691 machine held 31-33 GB, so this speaks up there. `0` disables.
pub const DEFAULT_CACHE_WARN_BYTES: u64 = 20 * 1024 * 1024 * 1024;

/// `<home>/.clud/cache`: the parent of `tools::clud_uv_cache_dir`.
pub fn clud_cache_dir_for(home: &Path) -> PathBuf {
    home.join(".clud").join("cache")
}

/// `<home>/.clud/cache-size.json`.
pub fn clud_cache_size_path_for(home: &Path) -> PathBuf {
    home.join(".clud").join(CLUD_CACHE_SIZE_FILE_NAME)
}

/// The one-line banner warning for `~/.clud/cache`.
pub fn clud_cache_banner_line(root: &Path, bytes: u64, warn_bytes: u64) -> String {
    banner_line_for("cache.warn_bytes", root, bytes, warn_bytes)
}

/// The `clud gc list` warning line for `~/.clud/cache`, or `None`.
/// `warn_bytes == 0` disables it.
pub fn clud_cache_list_warning(root: &Path, warn_bytes: u64, check: SizeCheck) -> Option<String> {
    if warn_bytes == 0 {
        return None;
    }
    match check {
        SizeCheck::Under(_) => None,
        SizeCheck::Over(bytes) => Some(format!(
            "warning: {} holds at least {bytes} bytes, over cache.warn_bytes ({warn_bytes}); \
             clud never deletes inside uv's cache — reclaim with \
             `UV_CACHE_DIR={} uv cache prune` or `clud gc purge --kind uv-cache --yes`",
            root.display(),
            root.join("uv").display()
        )),
        SizeCheck::Unknown => Some(format!(
            "warning: {} is too large to size within budget; it may exceed \
             cache.warn_bytes ({warn_bytes})",
            root.display()
        )),
    }
}

/// Daemon side for `~/.clud/cache`. Read-only walk; deletes nothing but a
/// stale cache file when the warning is disabled.
pub fn refresh_clud_cache_size(home: &Path, warn_bytes: u64, now_unix: i64) -> std::io::Result<()> {
    refresh_cache_at(
        &clud_cache_size_path_for(home),
        &clud_cache_dir_for(home),
        warn_bytes,
        now_unix,
    )
}

/// Launch side for `~/.clud/cache`, testable against any home.
pub fn clud_cache_launch_warning_at(home: &Path, now_unix: i64) -> Option<String> {
    let warn_bytes = crate::clud_settings::peek_cache_warn_bytes_at(home);
    if warn_bytes == 0 {
        return None;
    }
    let cached = read_cache(&clud_cache_size_path_for(home));
    match banner_decision(cached.as_ref(), warn_bytes, now_unix) {
        BannerVerdict::Warn { bytes } => Some(clud_cache_banner_line(
            &clud_cache_dir_for(home),
            bytes,
            warn_bytes,
        )),
        BannerVerdict::Skip(_) => None,
    }
}

/// Launch side for `~/.clud/cache`: never fails, never walks.
pub fn clud_cache_launch_warning() -> Option<String> {
    let home = crate::home::user_home()?;
    clud_cache_launch_warning_at(&home, now_unix())
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Launch side: the banner line, if any. Never fails, never walks.
pub fn launch_warning() -> Option<String> {
    let home = crate::home::user_home()?;
    launch_warning_at(&home, now_unix())
}

#[cfg(test)]
#[path = "worktree_size_cache_tests.rs"]
mod tests;
