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

use super::{
    apply_lock_prefix, decide_action, locked_removal_prefix, Action, CleanOptions, StalenessInputs,
    WorktreeStatus,
};

/// Decide one non-main worktree. Precedence, first match wins:
///
/// | condition                                        | action                          |
/// |--------------------------------------------------|---------------------------------|
/// | lock too fresh (live/dead/no pid under hard age) | legacy skip (`locked ...`)      |
/// | verdict `reclaimable`, status not `dirty`        | `Reclaim(<verdict reason>)`     |
/// | any other verdict, legacy skips                  | legacy skip + `; verdict: <r>`  |
/// | any other verdict / no verdict                   | legacy action, unchanged        |
///
/// So the verdict only ever *adds* removals backed by positive landing
/// evidence; every spare, `--force` and `--stale-after` outcome of the
/// ancestry rules is untouched, and a skip names the verdict so
/// `--dry-run` shows why. A `reclaimable` verdict never needs `--force`:
/// the executor it routes to refuses to force anything (DD-123).
pub(super) fn decide_with_verdict(
    inputs: StalenessInputs,
    verdict: Option<&RepoWorktreeVerdict>,
    opts: &CleanOptions,
) -> Action {
    let legacy = decide_action(inputs, opts);
    let Some(verdict) = verdict else {
        return legacy;
    };
    let lock_prefix = match locked_removal_prefix(inputs) {
        Ok(prefix) => prefix,
        Err(_) => return legacy,
    };
    if verdict.state == RepoWorktreeState::Reclaimable && inputs.status != WorktreeStatus::Dirty {
        return apply_lock_prefix(Action::Reclaim(verdict.reason.clone()), lock_prefix);
    }
    match legacy {
        Action::Skip(reason) => Action::Skip(format!("{reason}; verdict: {}", verdict.reason)),
        other => other,
    }
}

/// Issue #1648: decide a worktree whose verdict missed the CLI's deadline.
/// The ancestry rules decide alone, exactly as before #1606, and the
/// outcome says so:
///
/// | ancestry action | action                              |
/// |-----------------|-------------------------------------|
/// | `Skip(r)`       | `Skip("<r>; verdict timed out")`    |
/// | `Ignore`        | `Skip("verdict timed out")`         |
/// | `Remove(r)`     | `Remove(r)` (pre-#1606 behavior)    |
///
/// A missing verdict can never produce `Reclaim`, so it never adds a
/// removal; it only withholds one.
pub(super) fn decide_verdict_timed_out(inputs: StalenessInputs, opts: &CleanOptions) -> Action {
    const TIMED_OUT: &str = "verdict timed out";
    match decide_action(inputs, opts) {
        Action::Skip(reason) => Action::Skip(format!("{reason}; {TIMED_OUT}")),
        Action::Ignore => Action::Skip(TIMED_OUT.to_string()),
        other => other,
    }
}

/// How the CLI reports one executor outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Tally {
    Removed(String),
    Skipped(String),
    Failed(String),
}

/// Map an executor outcome to the CLI's report. A veto at re-verification
/// (including a worktree a running daemon reclaimed first, DD-129) is a
/// skip, not a failure: nothing was touched and a re-run is safe.
pub(super) fn tally_reclaim(outcome: ReclaimOutcome) -> Tally {
    match outcome {
        ReclaimOutcome::Removed { notes } => Tally::Removed(notes.join("; ")),
        ReclaimOutcome::Spared(reason) => {
            Tally::Skipped(format!("verdict changed before removal: {reason}"))
        }
        ReclaimOutcome::Failed(err) => Tally::Failed(err),
    }
}

#[cfg(test)]
#[path = "worktrees_verdict_tests.rs"]
mod tests;
