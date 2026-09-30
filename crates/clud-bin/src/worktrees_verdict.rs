//! Issue #1606: `--clean-worktrees` consults the daemon's squash-aware
//! verdict (`repo_worktree_verdict`, DD-122) before its own ancestry-based
//! status, so the CLI and the daemon agree on what landed.
//!
//! Pure: every decision here is a function of injected facts (the ancestry
//! status, the lock state and the verdict row), so the precedence is a
//! decision table in `worktrees_verdict_tests.rs` with no git and no
//! process table. See `docs/architecture/gc-and-registry.md#--clean-worktrees`
//! and DD-129.

use crate::daemon::repo_worktree_cli::{ReclaimOutcome, RepoWorktreeState, RepoWorktreeVerdict};

use super::{decide_action, Action, CleanOptions, StalenessInputs};

/// Decide one non-main worktree (RED stub: ignores the verdict).
pub(super) fn decide_with_verdict(
    inputs: StalenessInputs,
    verdict: Option<&RepoWorktreeVerdict>,
    opts: &CleanOptions,
) -> Action {
    let _ = (verdict, RepoWorktreeState::Reclaimable);
    decide_action(inputs, opts)
}

/// How the CLI reports one executor outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Tally {
    Removed(String),
    Skipped(String),
    Failed(String),
}

/// RED stub: treats every non-removal as a failure.
pub(super) fn tally_reclaim(outcome: ReclaimOutcome) -> Tally {
    match outcome {
        ReclaimOutcome::Removed { notes } => Tally::Removed(notes.join(", ")),
        ReclaimOutcome::Spared(r) | ReclaimOutcome::Failed(r) => Tally::Failed(r),
    }
}

#[cfg(test)]
#[path = "worktrees_verdict_tests.rs"]
mod tests;
