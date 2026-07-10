//! Rust `target/` reclamation — issue #510.
//!
//! `~/dev/**/target` is the single largest disk sink on a machine that
//! builds a lot of Rust, and clud's redb GC never sees it (it only tracks
//! worktrees / clones / extern-repos / trash / uv-cache). This module adds a
//! bounded, opt-in sweep: given a list of dev roots, find `target/` dirs that
//! sit next to a `Cargo.toml` and remove the ones whose mtime is older than a
//! threshold.
//!
//! Safety posture (deliberately conservative — this deletes regenerable build
//! output, but a mistaken delete still costs a rebuild):
//! - **Opt-in.** With no roots configured the sweep is a no-op. Enabled via
//!   `CLUD_GC_TARGET_ROOTS` (see `daemon/target_sweep.rs`).
//! - **Bounded discovery.** The walk is depth-limited and never crosses into
//!   `.git` / `node_modules` / a `target/` it already found. It never runs
//!   over the whole filesystem — only the explicitly configured roots.
//! - **mtime gate.** A `target/` touched within the threshold (default 14d)
//!   is left alone. A dir untouched that long is not part of an active build,
//!   which is the cheap stand-in for "no live session owns this".

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Default staleness gate. Longer than the 48h session-temp / worktree
/// policies because rebuilding `target/` is genuinely expensive, so we err
/// toward keeping it.
pub const DEFAULT_STALE_DAYS: u64 = 14;

/// How deep the discovery walk descends from each configured root before
/// giving up. Deep enough for `root/<repo>/<crate>/target` layouts, shallow
/// enough to stay cheap.
pub const MAX_DEPTH: usize = 6;

/// Directory names we never descend into during discovery.
const SKIP_DIRS: &[&str] = &[".git", "node_modules", ".claude"];

/// Outcome of a sweep. `bytes_freed` is best-effort (summed before removal).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SweepReport {
    pub targets_removed: usize,
    pub bytes_freed: u64,
    pub skipped: usize,
    pub dry_run: bool,
}

/// Discover `target/` directories under `root` (bounded by [`MAX_DEPTH`]).
/// A directory qualifies when it is named `target` and has a sibling
/// `Cargo.toml`. The walk does not descend into a qualifying `target/`.
pub fn find_target_dirs(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(root, 0, &mut found);
    found
}

fn walk(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if SKIP_DIRS.contains(&name.as_ref()) {
            continue;
        }
        if name == "target" && path.with_file_name("Cargo.toml").exists() {
            // `path.with_file_name("Cargo.toml")` resolves to the sibling of
            // `target/` — i.e. `<crate>/Cargo.toml`. Qualifies; don't recurse
            // into build output.
            found.push(path);
            continue;
        }
        walk(&path, depth + 1, found);
    }
}

/// Sweep every configured root, removing stale `target/` dirs. `threshold`
/// is the age gate; `now` is injectable for tests. No roots ⇒ empty report.
pub fn sweep_roots_at(
    roots: &[PathBuf],
    now: SystemTime,
    threshold: Duration,
    dry_run: bool,
) -> SweepReport {
    let mut report = SweepReport {
        dry_run,
        ..Default::default()
    };
    for root in roots {
        for target in find_target_dirs(root) {
            let Ok(meta) = fs::metadata(&target) else {
                continue;
            };
            let Ok(mtime) = meta.modified() else { continue };
            // Clock skew (future mtime) → skip; treat as fresh.
            let Ok(age) = now.duration_since(mtime) else {
                continue;
            };
            if age <= threshold {
                continue;
            }
            let bytes = dir_size(&target);
            if dry_run {
                report.targets_removed += 1;
                report.bytes_freed = report.bytes_freed.saturating_add(bytes);
                continue;
            }
            match fs::remove_dir_all(&target) {
                Ok(()) => {
                    report.targets_removed += 1;
                    report.bytes_freed = report.bytes_freed.saturating_add(bytes);
                }
                // Non-fatal (Windows lock, races). Retried next sweep.
                Err(_) => report.skipped += 1,
            }
        }
    }
    report
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total = total.saturating_add(dir_size(&entry.path()));
        } else {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::time::Duration as StdDuration;
    use tempfile::tempdir;

    /// Build `<root>/<crate_name>/{Cargo.toml, target/payload.o}` and return
    /// the target dir path.
    fn make_crate_with_target(root: &Path, crate_name: &str) -> PathBuf {
        let crate_dir = root.join(crate_name);
        fs::create_dir_all(&crate_dir).unwrap();
        File::create(crate_dir.join("Cargo.toml"))
            .unwrap()
            .write_all(b"[package]\nname = \"x\"\n")
            .unwrap();
        let target = crate_dir.join("target");
        fs::create_dir_all(&target).unwrap();
        File::create(target.join("payload.o"))
            .unwrap()
            .write_all(b"0123456789")
            .unwrap();
        target
    }

    #[test]
    fn find_discovers_target_next_to_cargo_toml() {
        let tmp = tempdir().unwrap();
        let target = make_crate_with_target(tmp.path(), "mycrate");
        let found = find_target_dirs(tmp.path());
        assert_eq!(found, vec![target]);
    }

    #[test]
    fn find_ignores_target_without_sibling_cargo_toml() {
        let tmp = tempdir().unwrap();
        // A bare `target/` with no Cargo.toml sibling must not match.
        fs::create_dir_all(tmp.path().join("weird").join("target")).unwrap();
        let found = find_target_dirs(tmp.path());
        assert!(found.is_empty());
    }

    #[test]
    fn find_does_not_descend_into_skip_dirs() {
        let tmp = tempdir().unwrap();
        // A target buried under node_modules should be skipped entirely.
        let nm = tmp.path().join("node_modules").join("pkg");
        fs::create_dir_all(&nm).unwrap();
        File::create(nm.join("Cargo.toml")).unwrap();
        fs::create_dir_all(nm.join("target")).unwrap();
        let found = find_target_dirs(tmp.path());
        assert!(found.is_empty());
    }

    #[test]
    fn sweep_no_roots_is_empty() {
        let report = sweep_roots_at(&[], SystemTime::now(), Duration::from_secs(1), false);
        assert_eq!(report, SweepReport::default());
    }

    #[test]
    fn sweep_leaves_fresh_targets() {
        let tmp = tempdir().unwrap();
        let target = make_crate_with_target(tmp.path(), "c");
        let report = sweep_roots_at(
            &[tmp.path().to_path_buf()],
            SystemTime::now(),
            Duration::from_secs(14 * 24 * 60 * 60),
            false,
        );
        assert_eq!(report.targets_removed, 0);
        assert!(target.exists());
    }

    #[test]
    fn sweep_removes_stale_targets_and_counts_bytes() {
        let tmp = tempdir().unwrap();
        let target = make_crate_with_target(tmp.path(), "c");
        let future_now = SystemTime::now() + StdDuration::from_secs(15 * 24 * 60 * 60);
        let report = sweep_roots_at(
            &[tmp.path().to_path_buf()],
            future_now,
            Duration::from_secs(14 * 24 * 60 * 60),
            false,
        );
        assert_eq!(report.targets_removed, 1);
        assert!(report.bytes_freed >= 10, "should count the 10-byte payload");
        assert!(!target.exists());
    }

    #[test]
    fn sweep_dry_run_does_not_delete() {
        let tmp = tempdir().unwrap();
        let target = make_crate_with_target(tmp.path(), "c");
        let future_now = SystemTime::now() + StdDuration::from_secs(15 * 24 * 60 * 60);
        let report = sweep_roots_at(
            &[tmp.path().to_path_buf()],
            future_now,
            Duration::from_secs(14 * 24 * 60 * 60),
            true,
        );
        assert_eq!(report.targets_removed, 1);
        assert!(target.exists(), "dry run must not delete");
    }

    #[test]
    fn default_stale_days_is_14() {
        assert_eq!(DEFAULT_STALE_DAYS, 14);
    }
}
