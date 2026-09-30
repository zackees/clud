//! Issue #1606: the narrow bridge `clud --clean-worktrees`
//! (`crate::worktrees`) uses to share the daemon's squash-aware verdict
//! (`repo_worktree_verdict`), its fact gathering (`repo_worktree_probe`) and
//! its reclaim executor (`repo_worktree_reclaim_exec`), so the CLI and the
//! daemon agree on what landed and remove it the same way.
//!
//! Nothing here decides anything: the decision stays the pure verdict, and
//! the CLI's own precedence lives in `worktrees_verdict.rs`.

use std::path::{Path, PathBuf};

pub(crate) use super::repo_worktree::{RepoWorktreeState, RepoWorktreeVerdict};
pub(crate) use super::repo_worktree_probe::{lookup_prs, PrRecord, RepoWorktreeRow};
pub(crate) use super::repo_worktree_reclaim::ProcessCwdSnapshot;
pub(crate) use super::repo_worktree_reclaim_exec::ReclaimOutcome;

use super::repo_worktree_probe::{collect_process_cwds, probe_repo_with};
use super::repo_worktree_reclaim_exec::{run_reclaim_with, ReclaimJob};

/// The live process table, as the daemon reads it.
pub(crate) fn live_process_cwds() -> ProcessCwdSnapshot {
    collect_process_cwds()
}

/// Verdict rows for every worktree of `main_repo`. The CLI has no session
/// registry, so it relies on the process table alone for "process inside".
pub(crate) fn probe_for_cli(
    main_repo: &Path,
    procs: &ProcessCwdSnapshot,
    prs: Option<&[PrRecord]>,
    wt_root: Option<&Path>,
) -> Vec<RepoWorktreeRow> {
    probe_repo_with(main_repo, &[], procs, prs, wt_root)
}

/// Remove one `reclaimable` row through the daemon's executor: fresh
/// re-probe and unchanged-verdict re-check, `git worktree remove` (never
/// `--force`), `branch -D` only at the verified tip, then prune. The CLI
/// never deletes a remote branch.
///
/// No cross-process lock with a running daemon (see DD-129): the re-probe
/// turns a worktree the daemon already reclaimed into
/// `Spared("gone from git worktree list")`, and git's own `.lock` files make
/// a truly simultaneous step fail cleanly rather than corrupt anything.
pub(crate) fn reclaim_for_cli(
    row: &RepoWorktreeRow,
    wt_root: Option<PathBuf>,
    lookup: &dyn Fn(&Path) -> Option<Vec<PrRecord>>,
    procs: &dyn Fn() -> ProcessCwdSnapshot,
) -> ReclaimOutcome {
    let job = ReclaimJob {
        row: row.clone(),
        delete_remote: false,
        session_cwds: Vec::new(),
        wt_root,
    };
    run_reclaim_with(&job, lookup, procs)
}
