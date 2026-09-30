//! Issue #1591: a squash-aware verdict for the worktrees of repos the GC
//! registry already knows about (`record_repo_visit`).
//!
//! This repo squash-merges, so a merged branch's tip is never an ancestor of
//! the default branch: `git branch --merged` and `merge-base --is-ancestor`
//! both answer "not merged" forever (DD-125). The verdict below instead
//! accepts only *positive* evidence that the work landed — a merged PR whose
//! head covers the local tip, or a patch-equivalent commit on the default
//! branch — and spares on every doubt.
//!
//! The function is pure over [`RepoWorktreeFacts`]: gathering the facts
//! (git, `gh`, lock liveness, session cwds) lives in `repo_worktree_probe`,
//! so the precedence here is unit-testable with no repo, no process table and
//! no network, per the reap/spare rule in `CLAUDE.md`.
//!
//! This PR is **read-only**: the verdict is surfaced by `clud gc list` and
//! nothing deletes on it yet (follow-up tracked in the PR description).

/// What the merged-PR lookup said about this worktree's branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PrFact {
    /// `gh` missing, unauthenticated, offline, timed out, or not a GitHub
    /// remote. Only the patch-equivalence fallback can prove anything.
    Unavailable,
    /// The lookup ran and no PR has this branch as its head.
    NoPr,
    /// At least one open PR has this branch as its head. Wins over any
    /// merged PR with the same head name (branch names get reused).
    Open { number: u64 },
    /// A merged PR had this head. `tip_covered` is whether the local tip is
    /// that PR's `headRefOid` or an ancestor of it; `None` when that could
    /// not be determined (for example the head object is not present
    /// locally).
    Merged {
        number: u64,
        tip_covered: Option<bool>,
    },
}

/// Every input the verdict needs, gathered by the probe. `Option<bool>`
/// fields are `None` when the underlying query failed; a failed query is
/// never read as the safe answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoWorktreeFacts {
    /// `false` until the probe has inspected this worktree at all.
    pub(crate) evaluated: bool,
    pub(crate) path_exists: bool,
    /// The repository's primary checkout (first `git worktree list` entry).
    pub(crate) is_main_checkout: bool,
    /// `locked` with a pid that is still alive.
    pub(crate) locked_live_pid: bool,
    /// A live session's cwd (or a process cwd) sits inside the worktree —
    /// this also covers "the currently checked-out one in any session".
    pub(crate) process_inside: bool,
    pub(crate) detached: bool,
    /// Uncommitted changes to tracked files.
    pub(crate) dirty: Option<bool>,
    /// Untracked, non-ignored files.
    pub(crate) untracked: Option<bool>,
    pub(crate) pr: PrFact,
    /// Fallback: the branch has at least one commit past its merge-base with
    /// the default branch, and that cumulative change appears verbatim as a
    /// commit on the default branch. Never true for a branch with no
    /// commits of its own.
    pub(crate) patch_landed: Option<bool>,
}

/// Displayed class, matching `clud gc list`'s `state` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepoWorktreeState {
    Reclaimable,
    Pinned,
    Dangling,
}

impl RepoWorktreeState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Reclaimable => "reclaimable",
            Self::Pinned => "pinned",
            Self::Dangling => "dangling",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoWorktreeVerdict {
    pub(crate) state: RepoWorktreeState,
    pub(crate) reason: String,
}

impl RepoWorktreeVerdict {
    fn pinned(reason: impl Into<String>) -> Self {
        Self {
            state: RepoWorktreeState::Pinned,
            reason: reason.into(),
        }
    }

    fn reclaimable(reason: impl Into<String>) -> Self {
        Self {
            state: RepoWorktreeState::Reclaimable,
            reason: reason.into(),
        }
    }
}

/// Decide one worktree. Precedence, first match wins:
///
/// | condition                                  | state       | reason                     |
/// |--------------------------------------------|-------------|----------------------------|
/// | not evaluated                              | pinned      | `no verdict yet`           |
/// | path missing                               | dangling    | `path missing`             |
/// | main checkout                              | pinned      | `main checkout`            |
/// | locked by a live pid                       | pinned      | `locked by live pid`       |
/// | a process/session inside                   | pinned      | `process inside`           |
/// | detached HEAD                              | pinned      | `detached`                 |
/// | dirty/untracked unknown                    | pinned      | `unverifiable`             |
/// | dirty                                      | pinned      | `dirty`                    |
/// | untracked files                            | pinned      | `untracked`                |
/// | open PR #N                                 | pinned      | `open PR #N`               |
/// | merged PR #N, tip covered                  | reclaimable | `merged via PR #N`         |
/// | merged PR #N, tip not covered              | pinned      | `commits after merge`      |
/// | merged PR #N, coverage unknown             | pinned      | `unverifiable`             |
/// | no PR / lookup unavailable, patch landed   | reclaimable | `landed by patch match`    |
/// | no PR, no patch match                      | pinned      | `no PR`                    |
/// | lookup unavailable, no patch match         | pinned      | `unverifiable`             |
pub(crate) fn repo_worktree_verdict(facts: &RepoWorktreeFacts) -> RepoWorktreeVerdict {
    let _ = facts;
    RepoWorktreeVerdict::pinned("unimplemented")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clean, idle, attached worktree whose branch is squash-merged via a
    /// PR that covers its tip. Each test flips exactly one fact.
    fn merged() -> RepoWorktreeFacts {
        RepoWorktreeFacts {
            evaluated: true,
            path_exists: true,
            is_main_checkout: false,
            locked_live_pid: false,
            process_inside: false,
            detached: false,
            dirty: Some(false),
            untracked: Some(false),
            pr: PrFact::Merged {
                number: 1551,
                tip_covered: Some(true),
            },
            // Squash-merged: the tip is NOT an ancestor of main, so there is
            // no ancestry signal anywhere in these facts — only the PR.
            patch_landed: Some(false),
        }
    }

    fn assert_verdict(facts: RepoWorktreeFacts, state: RepoWorktreeState, reason: &str) {
        assert_eq!(
            repo_worktree_verdict(&facts),
            RepoWorktreeVerdict {
                state,
                reason: reason.to_string()
            },
            "facts: {facts:?}"
        );
    }

    use RepoWorktreeState::{Dangling, Pinned, Reclaimable};

    // ---- Spare cases first: every guard asserts spare + reason. ----

    #[test]
    fn no_verdict_yet_is_spared() {
        let facts = RepoWorktreeFacts {
            evaluated: false,
            ..merged()
        };
        assert_verdict(facts, Pinned, "no verdict yet");
    }

    #[test]
    fn dirty_is_spared() {
        assert_verdict(
            RepoWorktreeFacts {
                dirty: Some(true),
                ..merged()
            },
            Pinned,
            "dirty",
        );
    }

    #[test]
    fn untracked_is_spared() {
        assert_verdict(
            RepoWorktreeFacts {
                untracked: Some(true),
                ..merged()
            },
            Pinned,
            "untracked",
        );
    }

    #[test]
    fn unreadable_status_is_spared_as_unverifiable() {
        assert_verdict(
            RepoWorktreeFacts {
                dirty: None,
                ..merged()
            },
            Pinned,
            "unverifiable",
        );
        assert_verdict(
            RepoWorktreeFacts {
                untracked: None,
                ..merged()
            },
            Pinned,
            "unverifiable",
        );
    }

    #[test]
    fn commits_after_merge_are_spared() {
        assert_verdict(
            RepoWorktreeFacts {
                pr: PrFact::Merged {
                    number: 1551,
                    tip_covered: Some(false),
                },
                // Even a patch match must not override "the tip holds
                // commits the merged PR never saw".
                patch_landed: Some(true),
                ..merged()
            },
            Pinned,
            "commits after merge",
        );
    }

    #[test]
    fn merged_pr_with_unknown_coverage_is_spared() {
        assert_verdict(
            RepoWorktreeFacts {
                pr: PrFact::Merged {
                    number: 1551,
                    tip_covered: None,
                },
                ..merged()
            },
            Pinned,
            "unverifiable",
        );
    }

    #[test]
    fn open_pr_is_spared() {
        assert_verdict(
            RepoWorktreeFacts {
                pr: PrFact::Open { number: 1580 },
                patch_landed: Some(true),
                ..merged()
            },
            Pinned,
            "open PR #1580",
        );
    }

    #[test]
    fn no_pr_and_no_patch_match_is_spared() {
        assert_verdict(
            RepoWorktreeFacts {
                pr: PrFact::NoPr,
                patch_landed: Some(false),
                ..merged()
            },
            Pinned,
            "no PR",
        );
        assert_verdict(
            RepoWorktreeFacts {
                pr: PrFact::NoPr,
                patch_landed: None,
                ..merged()
            },
            Pinned,
            "no PR",
        );
    }

    #[test]
    fn gh_unavailable_without_patch_match_is_unverifiable() {
        for patch_landed in [Some(false), None] {
            assert_verdict(
                RepoWorktreeFacts {
                    pr: PrFact::Unavailable,
                    patch_landed,
                    ..merged()
                },
                Pinned,
                "unverifiable",
            );
        }
    }

    #[test]
    fn live_lock_is_spared() {
        assert_verdict(
            RepoWorktreeFacts {
                locked_live_pid: true,
                ..merged()
            },
            Pinned,
            "locked by live pid",
        );
    }

    #[test]
    fn process_inside_is_spared() {
        assert_verdict(
            RepoWorktreeFacts {
                process_inside: true,
                ..merged()
            },
            Pinned,
            "process inside",
        );
    }

    #[test]
    fn detached_head_is_spared() {
        assert_verdict(
            RepoWorktreeFacts {
                detached: true,
                ..merged()
            },
            Pinned,
            "detached",
        );
    }

    #[test]
    fn main_checkout_is_spared() {
        assert_verdict(
            RepoWorktreeFacts {
                is_main_checkout: true,
                ..merged()
            },
            Pinned,
            "main checkout",
        );
    }

    #[test]
    fn missing_path_is_dangling() {
        assert_verdict(
            RepoWorktreeFacts {
                path_exists: false,
                ..merged()
            },
            Dangling,
            "path missing",
        );
    }

    /// Precedence: a live holder outranks dirtiness, which outranks the PR.
    #[test]
    fn guards_apply_in_documented_order() {
        assert_verdict(
            RepoWorktreeFacts {
                evaluated: false,
                path_exists: false,
                ..merged()
            },
            Pinned,
            "no verdict yet",
        );
        assert_verdict(
            RepoWorktreeFacts {
                process_inside: true,
                dirty: Some(true),
                ..merged()
            },
            Pinned,
            "process inside",
        );
        assert_verdict(
            RepoWorktreeFacts {
                dirty: Some(true),
                pr: PrFact::Open { number: 7 },
                ..merged()
            },
            Pinned,
            "dirty",
        );
    }

    // ---- Reclaim cases. ----

    /// The RED test of #1591: the branch is NOT an ancestor of main (squash
    /// merge; `patch_landed` is false too), yet the merged PR whose head
    /// covers the tip proves the work landed.
    #[test]
    fn squash_merged_branch_is_reclaimable_without_ancestry() {
        assert_verdict(merged(), Reclaimable, "merged via PR #1551");
    }

    #[test]
    fn gh_unavailable_but_patch_landed_is_reclaimable() {
        assert_verdict(
            RepoWorktreeFacts {
                pr: PrFact::Unavailable,
                patch_landed: Some(true),
                ..merged()
            },
            Reclaimable,
            "landed by patch match",
        );
    }

    #[test]
    fn no_pr_but_patch_landed_is_reclaimable() {
        assert_verdict(
            RepoWorktreeFacts {
                pr: PrFact::NoPr,
                patch_landed: Some(true),
                ..merged()
            },
            Reclaimable,
            "landed by patch match",
        );
    }

    #[test]
    fn state_names_match_gc_list_json() {
        assert_eq!(Reclaimable.as_str(), "reclaimable");
        assert_eq!(Pinned.as_str(), "pinned");
        assert_eq!(Dangling.as_str(), "dangling");
    }
}
