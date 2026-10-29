//! Issue #2302: reclaim cargo build output before trashing a whole worktree.
//!
//! About 77% of `~/.clud/worktrees` is `target/`. When the root is over
//! `worktrees.warn_bytes`, each idle, not-in-use large worktree has its
//! `target/` dirs (top-level and nested ones beside a `Cargo.toml`) pruned with
//! `soldr cache prune-target <target> --force`, which deletes only superseded
//! artifacts. Git state and source are never touched. A missing `soldr` or a
//! failed prune is logged and the worktree falls through to the warn-then-trash
//! path unchanged.
//!
//! [`select`] and [`prune_worktree`] are pure over injected facts (candidate
//! facts, a runner, a sizer); the daemon wires in the real `soldr` runner.

use std::path::{Path, PathBuf};

use crate::gc::target_sweep::{dir_size, find_target_dirs};
use crate::gc::worktree_schedule::{Candidate, MIN_IDLE_SECS};

/// Result of pruning one worktree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneOutcome {
    pub reclaimed_bytes: u64,
    pub targets_pruned: usize,
    pub targets_failed: usize,
}

/// Worktrees to prune this tick: root over budget, idle long enough, and not
/// exempt (kept, reservation, live session/cwd, source in use). Largest first
/// is the caller's order and is preserved.
pub fn select(over: bool, candidates: &[Candidate]) -> Vec<&Candidate> {
    if !over {
        return Vec::new();
    }
    candidates
        .iter()
        .filter(|c| !c.exempt && c.idle_secs >= MIN_IDLE_SECS)
        .collect()
}

/// Prune every `target/` under `worktree` through `run`, measuring each with
/// `size`. A `run` error counts as a failed target and never aborts the rest.
pub fn prune_worktree(
    worktree: &Path,
    run: &mut dyn FnMut(&Path) -> Result<(), String>,
    size: &dyn Fn(&Path) -> u64,
) -> PruneOutcome {
    let mut out = PruneOutcome::default();
    let targets: Vec<PathBuf> = find_target_dirs(worktree);
    for target in targets {
        let before = size(&target);
        match run(&target) {
            Ok(()) => {
                out.targets_pruned += 1;
                out.reclaimed_bytes = out
                    .reclaimed_bytes
                    .saturating_add(before.saturating_sub(size(&target)));
            }
            Err(e) => {
                out.targets_failed += 1;
                eprintln!(
                    "[clud] gc: soldr prune-target failed for {}: {e}",
                    target.display()
                );
            }
        }
    }
    out
}

/// Real runner: `soldr cache prune-target <target> --force`. `Err` when
/// soldr is not installed or exits non-zero.
pub fn soldr_runner() -> Result<impl FnMut(&Path) -> Result<(), String>, String> {
    let soldr = which::which("soldr").map_err(|e| format!("soldr not found: {e}"))?;
    Ok(move |target: &Path| {
        let argv = vec![
            soldr.to_string_lossy().into_owned(),
            "cache".into(),
            "prune-target".into(),
            target.to_string_lossy().into_owned(),
            "--force".into(),
        ];
        match crate::backend_bootstrap::run_captured_command(argv)? {
            (0, _) => Ok(()),
            (code, out) => Err(format!("exit {code}: {}", out.trim())),
        }
    })
}

/// Production sizer.
pub fn real_size(path: &Path) -> u64 {
    dir_size(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const DAY: i64 = 24 * 60 * 60;

    fn cand(path: &str, idle: i64, exempt: bool) -> Candidate {
        Candidate {
            path: path.into(),
            bytes: 1,
            idle_secs: idle,
            exempt,
        }
    }

    #[test]
    fn select_skips_in_use_recent_and_under_budget() {
        let c = [
            cand("/a", 2 * DAY, false),
            cand("/b", 2 * DAY, true),
            cand("/c", DAY - 1, false),
        ];
        let got: Vec<_> = select(true, &c).iter().map(|c| c.path.as_str()).collect();
        assert_eq!(got, ["/a"]);
        assert!(select(false, &c).is_empty());
    }

    fn make_target(dir: &Path, bytes: usize) {
        fs::create_dir_all(dir.join("target/debug")).unwrap();
        fs::write(dir.join("Cargo.toml"), "").unwrap();
        fs::write(dir.join("target/debug/big"), vec![0u8; bytes]).unwrap();
    }

    #[test]
    fn prunes_top_level_and_nested_targets_and_sums_reclaimed() {
        let wt = tempfile::tempdir().unwrap();
        make_target(wt.path(), 1000);
        make_target(&wt.path().join("nested"), 500);
        let mut seen = Vec::new();
        let mut run = |t: &Path| {
            seen.push(t.to_path_buf());
            fs::write(t.join("debug/big"), b"x").unwrap(); // shrink to 1 byte
            Ok(())
        };
        let out = prune_worktree(wt.path(), &mut run, &dir_size);
        assert_eq!(seen.len(), 2);
        assert_eq!(out.targets_pruned, 2);
        assert_eq!(out.reclaimed_bytes, 999 + 499);
        assert!(wt.path().join("Cargo.toml").exists());
    }

    #[test]
    fn a_failed_prune_is_counted_and_reclaims_nothing() {
        let wt = tempfile::tempdir().unwrap();
        make_target(wt.path(), 100);
        let mut run = |_: &Path| Err("boom".to_string());
        let out = prune_worktree(wt.path(), &mut run, &dir_size);
        assert_eq!(
            out,
            PruneOutcome {
                reclaimed_bytes: 0,
                targets_pruned: 0,
                targets_failed: 1
            }
        );
    }

    #[test]
    fn target_without_cargo_toml_is_left_alone() {
        let wt = tempfile::tempdir().unwrap();
        fs::create_dir_all(wt.path().join("target")).unwrap();
        let mut run = |_: &Path| -> Result<(), String> { panic!("must not run") };
        assert_eq!(
            prune_worktree(wt.path(), &mut run, &dir_size),
            PruneOutcome::default()
        );
    }
}
