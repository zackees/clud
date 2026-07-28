//! Which `CLUD:<pid>`-tagged cohorts a periodic sweep may reap.
//!
//! Issue #465, layer 2 of #340. Every process clud spawns carries
//! `RUNNING_PROCESS_ORIGINATOR=CLUD:<originator_pid>`. When the originator
//! dies without cleaning up — panic, `SIGKILL`, terminal closed before the
//! `Drop` guard runs, or simply one of the many foreground invocations that
//! never involve the daemon — its descendants are orphaned and stay that way.
//! One observed workstation snapshot had 107 such processes across 18 dead
//! cohorts.
//!
//! **This module decides; it does not kill.** The decision is a pure function
//! over a snapshot so every safety rule can be tested exhaustively without
//! putting a real process at risk. Wiring it to an actual sweep loop is
//! deliberately a separate change: the issue's own framing is that a mistake
//! here is a worse regression than the leak, and the failure is unrecoverable
//! — a wrongly reaped subtree cannot be un-killed.
//!
//! The four exclusions below are each a distinct way an innocent subtree can
//! look exactly like a leak.

use std::collections::{HashMap, HashSet};

/// How long a freshly orphaned cohort is left alone (issue #465, rule 4).
///
/// A clud that just exited may have a `Drop` guard mid-reap. Layer 1 owns
/// that cleanup; a sweep racing it would kill processes out from under the
/// guard and confuse both sets of logs. Waiting costs one tick.
pub const ORPHAN_GRACE_MS: u64 = 10_000;

/// One tagged process as seen by the sampler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaggedProcess {
    pub pid: u32,
    /// The `<pid>` from `RUNNING_PROCESS_ORIGINATOR=CLUD:<pid>`.
    pub originator_pid: u32,
    /// When this process was first observed carrying the tag. Used for the
    /// grace window rather than the originator's death time, which is not
    /// observable once the process is gone.
    pub first_seen_ms: u64,
}

/// A dead-originator cohort judged safe to reap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReapableCohort {
    pub originator_pid: u32,
    pub descendants: Vec<u32>,
}

/// Inputs a sweep needs to make its decision, gathered by the caller so this
/// function stays pure and testable.
pub struct SweepContext<'a> {
    pub now_ms: u64,
    /// Is this originator PID still running?
    pub originator_is_live: &'a dyn Fn(u32) -> bool,
    /// Originators the user deliberately detached — see
    /// [`crate::handover_registry`].
    pub handover_protected: &'a HashSet<u32>,
    /// PIDs the daemon owns directly (itself, its workers) plus anything that
    /// declared itself a daemon. Never reapable, and their presence disowns
    /// the whole cohort.
    pub daemon_owned: &'a HashSet<u32>,
}

/// Select the cohorts a sweep may reap.
///
/// Excluded, in the order the issue states them:
///
/// 1. **Live originator** — not an orphan at all; the owner is still there.
/// 2. **Handover-registered** — a detached session, whose whole point is to
///    outlive its originator. Indistinguishable from a leak by inspection,
///    which is why the registry has to be persisted.
/// 3. **Daemon-owned** — the daemon and its workers are reached through a
///    different ownership path. A cohort containing one is skipped *entirely*
///    rather than filtered: if a daemon-owned process is in the subtree, the
///    tree walk that would follow could reach the daemon through it, and
///    partial reaping of a cohort is a shape no caller expects.
/// 4. **Inside the grace window** — layer 1's `Drop` guard may still be
///    working; let it finish.
///
/// Cohorts are returned sorted by originator PID, and descendants sorted
/// within each, so callers and tests see a deterministic order.
pub fn select_reapable_cohorts(
    processes: &[TaggedProcess],
    ctx: &SweepContext<'_>,
) -> Vec<ReapableCohort> {
    let mut by_originator: HashMap<u32, Vec<&TaggedProcess>> = HashMap::new();
    for proc in processes {
        by_originator
            .entry(proc.originator_pid)
            .or_default()
            .push(proc);
    }

    let mut cohorts: Vec<ReapableCohort> = by_originator
        .into_iter()
        .filter(|(originator, members)| {
            !(ctx.handover_protected.contains(originator)
                || (ctx.originator_is_live)(*originator)
                || members.iter().any(|m| ctx.daemon_owned.contains(&m.pid))
                // Youngest member decides: as long as anything in the cohort
                // is still inside the window, layer 1 may yet be working on
                // it. Taking the oldest instead would let a long-lived parent
                // drag a just-spawned child into range of the sweep.
                || members.iter().any(|m| {
                    ctx.now_ms.saturating_sub(m.first_seen_ms) < ORPHAN_GRACE_MS
                }))
        })
        .map(|(originator_pid, members)| {
            let mut descendants: Vec<u32> = members.iter().map(|m| m.pid).collect();
            descendants.sort_unstable();
            ReapableCohort {
                originator_pid,
                descendants,
            }
        })
        .collect();

    cohorts.sort_unstable_by_key(|c| c.originator_pid);
    cohorts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, originator: u32, first_seen_ms: u64) -> TaggedProcess {
        TaggedProcess {
            pid,
            originator_pid: originator,
            first_seen_ms,
        }
    }

    const DEAD: &dyn Fn(u32) -> bool = &|_| false;
    const ALIVE: &dyn Fn(u32) -> bool = &|_| true;

    fn ctx<'a>(
        now_ms: u64,
        originator_is_live: &'a dyn Fn(u32) -> bool,
        handover: &'a HashSet<u32>,
        daemon: &'a HashSet<u32>,
    ) -> SweepContext<'a> {
        SweepContext {
            now_ms,
            originator_is_live,
            handover_protected: handover,
            daemon_owned: daemon,
        }
    }

    #[test]
    fn a_dead_originator_cohort_is_reapable() {
        let procs = vec![proc(100, 71584, 0), proc(101, 71584, 0)];
        let got = select_reapable_cohorts(
            &procs,
            &ctx(ORPHAN_GRACE_MS * 2, DEAD, &HashSet::new(), &HashSet::new()),
        );
        assert_eq!(
            got,
            vec![ReapableCohort {
                originator_pid: 71584,
                descendants: vec![100, 101],
            }]
        );
    }

    #[test]
    fn a_live_originator_is_never_touched() {
        let procs = vec![proc(100, 71584, 0)];
        let got = select_reapable_cohorts(
            &procs,
            &ctx(ORPHAN_GRACE_MS * 2, ALIVE, &HashSet::new(), &HashSet::new()),
        );
        assert!(got.is_empty(), "live originator must be skipped: {got:?}");
    }

    #[test]
    fn a_detached_session_survives_its_dead_originator() {
        // The regression that would hurt most: `--detach` means the user asked
        // for exactly this shape, and it is indistinguishable from a leak
        // without the registry.
        let procs = vec![proc(100, 71584, 0)];
        let protected: HashSet<u32> = [71584].into_iter().collect();
        let got = select_reapable_cohorts(
            &procs,
            &ctx(ORPHAN_GRACE_MS * 2, DEAD, &protected, &HashSet::new()),
        );
        assert!(got.is_empty(), "detached cohort must be spared: {got:?}");
    }

    #[test]
    fn a_cohort_containing_a_daemon_owned_pid_is_skipped_whole() {
        // Not merely filtered down to the other members: a tree walk from this
        // cohort could reach the daemon through the spared process, and a
        // half-reaped cohort is a shape no caller expects.
        let procs = vec![proc(100, 71584, 0), proc(101, 71584, 0)];
        let daemon: HashSet<u32> = [101].into_iter().collect();
        let got = select_reapable_cohorts(
            &procs,
            &ctx(ORPHAN_GRACE_MS * 2, DEAD, &HashSet::new(), &daemon),
        );
        assert!(got.is_empty(), "whole cohort must be skipped: {got:?}");
    }

    #[test]
    fn a_freshly_orphaned_cohort_waits_for_the_grace_window() {
        let procs = vec![proc(100, 71584, 1_000)];
        // 1_000 + grace - 1: still inside the window by one millisecond.
        let inside = select_reapable_cohorts(
            &procs,
            &ctx(
                1_000 + ORPHAN_GRACE_MS - 1,
                DEAD,
                &HashSet::new(),
                &HashSet::new(),
            ),
        );
        assert!(
            inside.is_empty(),
            "must wait out the grace window: {inside:?}"
        );

        // Exactly at the boundary it becomes eligible.
        let outside = select_reapable_cohorts(
            &procs,
            &ctx(
                1_000 + ORPHAN_GRACE_MS,
                DEAD,
                &HashSet::new(),
                &HashSet::new(),
            ),
        );
        assert_eq!(outside.len(), 1, "grace boundary is inclusive");
    }

    #[test]
    fn the_youngest_member_governs_the_grace_window() {
        // An old cohort that just gained a child is still settling. Using the
        // oldest member would let the sweep race a spawn in progress.
        let procs = vec![proc(100, 71584, 0), proc(101, 71584, 50_000)];
        let got =
            select_reapable_cohorts(&procs, &ctx(50_001, DEAD, &HashSet::new(), &HashSet::new()));
        assert!(got.is_empty(), "youngest member must govern: {got:?}");
    }

    #[test]
    fn cohorts_are_partitioned_by_originator_and_ordered() {
        let procs = vec![
            proc(300, 9, 0),
            proc(100, 5, 0),
            proc(200, 9, 0),
            proc(101, 5, 0),
        ];
        let got = select_reapable_cohorts(
            &procs,
            &ctx(ORPHAN_GRACE_MS * 2, DEAD, &HashSet::new(), &HashSet::new()),
        );
        assert_eq!(
            got,
            vec![
                ReapableCohort {
                    originator_pid: 5,
                    descendants: vec![100, 101],
                },
                ReapableCohort {
                    originator_pid: 9,
                    descendants: vec![200, 300],
                },
            ]
        );
    }

    #[test]
    fn one_cohort_being_spared_does_not_spare_the_others() {
        // The observed shape: 18 dead cohorts at once, only some protected.
        let procs = vec![proc(100, 5, 0), proc(200, 9, 0)];
        let protected: HashSet<u32> = [5].into_iter().collect();
        let got = select_reapable_cohorts(
            &procs,
            &ctx(ORPHAN_GRACE_MS * 2, DEAD, &protected, &HashSet::new()),
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].originator_pid, 9);
    }

    #[test]
    fn an_empty_snapshot_reaps_nothing() {
        let got = select_reapable_cohorts(
            &[],
            &ctx(ORPHAN_GRACE_MS * 2, DEAD, &HashSet::new(), &HashSet::new()),
        );
        assert!(got.is_empty());
    }
}
