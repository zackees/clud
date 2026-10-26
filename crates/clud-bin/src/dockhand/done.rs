//! When a workspace is done (#926 v2 §5).
//!
//! An explicit `dockhand done` marks a workspace done. Derived signals (the
//! worktree went missing, the branch is gone, the PR merged) share one
//! safeguard: they count only when the worktree is absent, or present but
//! clean with nothing unpushed. Dirty or unpushed work is never destroyed on
//! derived evidence. The caller gathers the facts; this decides.

/// What the maintenance pass observed about a workspace's checkout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checkout {
    /// The worktree exists. `clean` means no uncommitted changes and no
    /// commits missing from the upstream.
    Present { clean: bool, detached: bool },
    /// The worktree is gone. `parent_mounted` is whether the directory that
    /// held it exists, so an unmounted drive is never read as deletion;
    /// `observations` counts consecutive passes that saw it missing.
    Missing {
        parent_mounted: bool,
        observations: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Signals {
    /// `dockhand done` was run for this workspace.
    pub explicit: bool,
    /// The branch no longer exists locally or upstream.
    pub branch_gone: bool,
    /// A cached GitHub lookup says the branch's PR merged.
    pub pr_merged: bool,
}

/// Consecutive passes a missing worktree must be seen before it counts.
pub const MISSING_OBSERVATIONS: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Done {
    Yes(&'static str),
    No(&'static str),
}

pub fn decide(checkout: Checkout, signals: Signals) -> Done {
    if signals.explicit {
        return Done::Yes("marked done");
    }
    match checkout {
        Checkout::Missing {
            parent_mounted: false,
            ..
        } => Done::No("worktree's parent is not mounted"),
        Checkout::Missing { observations, .. } if observations < MISSING_OBSERVATIONS => {
            Done::No("worktree missing; awaiting a second observation")
        }
        Checkout::Missing { .. } => Done::Yes("worktree missing"),
        Checkout::Present { clean: false, .. } if signals.branch_gone || signals.pr_merged => {
            Done::No("dirty or unpushed work; derived signals ignored")
        }
        Checkout::Present { detached: true, .. } if !signals.pr_merged => {
            Done::No("detached HEAD; relies on leases and tier ageing")
        }
        Checkout::Present { .. } if signals.pr_merged => Done::Yes("PR merged; worktree clean"),
        Checkout::Present { .. } if signals.branch_gone => Done::Yes("branch gone; worktree clean"),
        Checkout::Present { .. } => Done::No("active workspace"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLEAN: Checkout = Checkout::Present {
        clean: true,
        detached: false,
    };
    const DIRTY: Checkout = Checkout::Present {
        clean: false,
        detached: false,
    };

    fn signals(branch_gone: bool, pr_merged: bool) -> Signals {
        Signals {
            explicit: false,
            branch_gone,
            pr_merged,
        }
    }

    fn is_done(checkout: Checkout, signals: Signals) -> bool {
        matches!(decide(checkout, signals), Done::Yes(_))
    }

    #[test]
    fn explicit_done_wins_even_over_dirty_work() {
        let explicit = Signals {
            explicit: true,
            ..Signals::default()
        };
        assert!(is_done(DIRTY, explicit));
    }

    #[test]
    fn derived_signals_never_destroy_dirty_or_unpushed_work() {
        assert!(!is_done(DIRTY, signals(true, false)));
        assert!(!is_done(DIRTY, signals(false, true)), "merged PRs included");
        assert!(!is_done(DIRTY, signals(true, true)));
    }

    #[test]
    fn derived_signals_finish_a_clean_workspace() {
        assert!(is_done(CLEAN, signals(true, false)));
        assert!(is_done(CLEAN, signals(false, true)));
        assert!(!is_done(CLEAN, Signals::default()));
    }

    #[test]
    fn a_missing_worktree_needs_a_mounted_parent_and_two_observations() {
        let missing = |parent_mounted, observations| Checkout::Missing {
            parent_mounted,
            observations,
        };
        assert!(
            !is_done(missing(false, 10), Signals::default()),
            "unmounted drive"
        );
        assert!(!is_done(missing(true, 1), Signals::default()));
        assert!(is_done(missing(true, 2), Signals::default()));
    }

    #[test]
    fn detached_head_is_not_branch_gone() {
        let detached = Checkout::Present {
            clean: true,
            detached: true,
        };
        assert!(!is_done(detached, signals(true, false)));
        assert!(is_done(detached, signals(false, true)));
    }
}
