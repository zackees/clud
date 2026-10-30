//! Size cap for the session temp dir `~/.clud/tmp` (#2397).
//!
//! The 72 h age sweep ([`super::session_tmp`]) was the only reclaimer, so a day
//! of heavy sessions parked tens of gigabytes until it aged out (64 GB on the
//! box that reported this). `tmp.max_bytes` (default 10 GiB, `0` disables)
//! adds a size trigger: when the tree is over the cap, the oldest *eviction
//! units* go first until it fits. This supersedes the warn-only stance of
//! DD-141 for the over-cap case.
//!
//! # Units
//!
//! The harness lays scratch out as `claude-<uid>/<project>/<session>/`, so the
//! unit of eviction mirrors the age sweep's depth rule
//! ([`super::session_tmp::MAX_NESTED_DEPTH`]): a directory at the deepest
//! level is one unit (a session's scratch), anything shallower is recursed
//! into, and a file at any level is its own unit. A top-level directory is
//! therefore never removed wholesale while it still holds a live session.
//!
//! # Liveness
//!
//! The age sweep has no session registry: it decides "live" purely by
//! activity, treating any tree with an mtime newer than its threshold as in
//! use, and every failure to establish idleness as "in use". Size eviction
//! applies the same signal with a much shorter window,
//! [`MIN_IDLE`]: a unit whose newest mtime anywhere beneath it is younger than
//! that is never evicted, an unreadable or oversized walk counts as live, and
//! the idleness is re-measured immediately before removal. A live session
//! writes constantly, so it always stays; the cost of a too-generous window is
//! only that the cap is exceeded until the unit goes quiet. Candidates are
//! sorted oldest first, so the first live unit ends the pass.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::delete_audit;
use super::session_tmp::MAX_NESTED_DEPTH;

/// Default `tmp.max_bytes`: 10 GiB.
pub const DEFAULT_TMP_MAX_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// A unit with any write this recent anywhere beneath it is a live session.
pub const MIN_IDLE: Duration = Duration::from_secs(6 * 60 * 60);

/// Entries one measurement walk may visit before the unit is assumed live.
const WALK_BUDGET: usize = 2_000_000;

const SITE: &str = "gc.session-tmp-cap";

/// Outcome of one [`enforce_cap_at`] pass.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CapReport {
    pub dry_run: bool,
    pub cap: u64,
    /// Measured size before eviction (apparent bytes).
    pub total_bytes: u64,
    /// Units removed (or, in a dry run, that would be).
    pub evicted: usize,
    pub reclaimed_bytes: u64,
    /// Units kept because they were, or might be, live.
    pub kept_live: usize,
    /// Units whose removal failed.
    pub failed: usize,
}

impl CapReport {
    /// The one line `clud gc prune` prints.
    pub fn summary_line(&self) -> String {
        if self.cap == 0 {
            return "tmp: size cap disabled (tmp.max_bytes = 0).".to_string();
        }
        if self.total_bytes <= self.cap {
            return format!(
                "tmp: {} within the {} cap.",
                super::cli::format_bytes(self.total_bytes),
                super::cli::format_bytes(self.cap)
            );
        }
        let verb = if self.dry_run {
            "--dry-run would evict"
        } else {
            "evicted"
        };
        format!(
            "tmp: {} over the {} cap; {verb} {} idle unit(s) ({}), kept {} live, {} failed.",
            super::cli::format_bytes(self.total_bytes),
            super::cli::format_bytes(self.cap),
            self.evicted,
            super::cli::format_bytes(self.reclaimed_bytes),
            self.kept_live,
            self.failed
        )
    }
}

#[derive(Debug)]
struct Unit {
    path: PathBuf,
    is_dir: bool,
    bytes: u64,
    /// Newest mtime in the unit; `None` when it could not be established.
    newest: Option<SystemTime>,
}

/// Apparent size and newest mtime of `path`'s subtree, without following
/// symlinks. `None` newest means "could not establish", i.e. treat as live.
fn measure(path: &Path, budget: &mut usize) -> (u64, Option<SystemTime>) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return (0, None);
    };
    let mut newest = meta.modified().ok();
    let mut bytes = if meta.is_dir() { 0 } else { meta.len() };
    if !meta.is_dir() {
        return (bytes, newest);
    }
    let Ok(entries) = fs::read_dir(path) else {
        return (bytes, None);
    };
    for entry in entries {
        if *budget == 0 {
            return (bytes, None);
        }
        *budget -= 1;
        let Ok(entry) = entry else {
            return (bytes, None);
        };
        let (child_bytes, child_newest) = measure(&entry.path(), budget);
        bytes = bytes.saturating_add(child_bytes);
        newest = match (newest, child_newest) {
            (Some(a), Some(b)) => Some(a.max(b)),
            _ => None,
        };
    }
    (bytes, newest)
}

fn collect_units(dir: &Path, depth: usize, units: &mut Vec<Unit>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() && depth + 1 < MAX_NESTED_DEPTH {
            collect_units(&path, depth + 1, units);
            continue;
        }
        let mut budget = WALK_BUDGET;
        let (bytes, newest) = measure(&path, &mut budget);
        units.push(Unit {
            path,
            is_dir: meta.is_dir(),
            bytes,
            newest,
        });
    }
}

/// Is the unit idle by [`MIN_IDLE`]? Any doubt answers `false` (live).
fn is_idle(newest: Option<SystemTime>, now: SystemTime) -> bool {
    newest
        .and_then(|mtime| now.duration_since(mtime).ok())
        .is_some_and(|age| age >= MIN_IDLE)
}

fn remove_unit(unit: &Unit) -> io::Result<()> {
    if unit.is_dir {
        crate::rm_tool::remove_dir_tree_writable(&unit.path)
    } else {
        crate::rm_tool::remove_link_or_file(&unit.path)
    }
}

/// Evict the oldest idle units under `root` until it is within `cap` bytes.
/// `cap == 0` disables. Missing `root` is an empty, in-cap tree.
pub fn enforce_cap_at(root: &Path, cap: u64, now: SystemTime, dry_run: bool) -> CapReport {
    let mut report = CapReport {
        dry_run,
        cap,
        ..Default::default()
    };
    if cap == 0 || !root.is_dir() {
        return report;
    }
    let mut units = Vec::new();
    collect_units(root, 0, &mut units);
    report.total_bytes = units.iter().fold(0u64, |t, u| t.saturating_add(u.bytes));
    if report.total_bytes <= cap {
        return report;
    }
    // Oldest first; an unmeasurable unit sorts last (it is live by default).
    units.sort_by_key(|u| (u.newest.is_none(), u.newest));
    let mut remaining = report.total_bytes;
    for unit in &units {
        if remaining <= cap {
            break;
        }
        if !is_idle(unit.newest, now) {
            report.kept_live += 1;
            continue;
        }
        // Re-measure right before acting: a session may have woken up.
        let mut budget = WALK_BUDGET;
        let (_, newest_now) = measure(&unit.path, &mut budget);
        if !is_idle(newest_now, now) {
            report.kept_live += 1;
            continue;
        }
        if !dry_run {
            delete_audit::record(SITE, &unit.path, "session-tmp over tmp.max_bytes");
            if remove_unit(unit).is_err() {
                report.failed += 1;
                continue;
            }
        }
        report.evicted += 1;
        report.reclaimed_bytes = report.reclaimed_bytes.saturating_add(unit.bytes);
        remaining = remaining.saturating_sub(unit.bytes);
    }
    report
}

/// Production entry: `~/.clud/tmp` against `tmp.max_bytes`.
pub fn enforce_cap(dry_run: bool) -> CapReport {
    let Some(root) = super::session_tmp::session_tmp_dir() else {
        return CapReport::default();
    };
    let cap = crate::clud_settings::load_tmp_max_bytes().unwrap_or(DEFAULT_TMP_MAX_BYTES);
    enforce_cap_at(&root, cap, SystemTime::now(), dry_run)
}

#[cfg(test)]
#[path = "session_tmp_cap_tests.rs"]
mod tests;
