//! Issue #1603 (the deletion half of #1591): pure decisions for reclaiming a
//! repo worktree the #1602 verdict called `reclaimable`.
//!
//! Every function here is a pure function over injected facts, per the
//! reap/spare rule in `CLAUDE.md`: selection on the registry worker, the
//! last-moment re-verification on the purge-pool thread, and the two
//! follow-on ref deletions. The side-effecting executor that gathers the
//! facts and runs `git` lives in `repo_worktree_reclaim_exec`. Every
//! "no" carries the reason that is logged, so a spare is always explained.

use std::path::{Path, PathBuf};

use super::repo_worktree::RepoWorktreeState;
use super::repo_worktree_probe::RepoWorktreeRow;

/// `CLUD_GC_REPO_WORKTREES`: what the daemon does with repo worktrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReclaimMode {
    /// `0`/`false`/`off`/`no`: no probe, no verdicts, no deletion.
    Off,
    /// `observe`/`dry-run`: probe and log what would be removed; delete
    /// nothing.
    Observe,
    /// Default (unset, `1`/`true`/`on`/`yes`): remove re-verified
    /// `reclaimable` worktrees on the tick after they are first seen.
    Delete,
}

/// Parse `CLUD_GC_REPO_WORKTREES`. An unrecognized value is doubt, and doubt
/// never deletes: it maps to `Observe`.
pub(crate) fn reclaim_mode_from_raw(raw: Option<&str>) -> ReclaimMode {
    match raw.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        None | Some("" | "1" | "true" | "on" | "yes") => ReclaimMode::Delete,
        Some("0" | "false" | "off" | "no") => ReclaimMode::Off,
        Some(_) => ReclaimMode::Observe,
    }
}

/// What the registry worker does with one cached row on a tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReclaimSelection {
    /// Hand it to the purge pool, which re-verifies before deleting.
    Dispatch,
    /// Observe mode: log it as a would-be removal, touch nothing.
    WouldRemove,
    /// Leave it. The reason is logged only for rows that were reclaimable.
    Spare(&'static str),
}

/// Worker-side selection over the cached snapshot (up to one tick old).
/// `live_now` is a live session cwd inside the path *now*; `in_flight` is a
/// reclaim of this path already queued on the pool.
pub(crate) fn reclaim_selection(
    mode: ReclaimMode,
    state: RepoWorktreeState,
    live_now: bool,
    in_flight: bool,
) -> ReclaimSelection {
    if state != RepoWorktreeState::Reclaimable {
        return ReclaimSelection::Spare("not reclaimable");
    }
    if mode == ReclaimMode::Off {
        return ReclaimSelection::Spare("repo-worktree gc off");
    }
    if live_now {
        return ReclaimSelection::Spare("process inside");
    }
    if in_flight {
        return ReclaimSelection::Spare("reclaim already in flight");
    }
    match mode {
        ReclaimMode::Observe => ReclaimSelection::WouldRemove,
        _ => ReclaimSelection::Dispatch,
    }
}

/// Last-moment re-check on the purge-pool thread (the #946 pattern).
/// `fresh` is the same worktree re-probed from scratch — fresh `git`, a fresh
/// PR lookup, a fresh process table — immediately before the destructive
/// call. Any difference from what authorized the dispatch vetoes it.
pub(crate) fn reverify_reclaim(
    cached: &RepoWorktreeRow,
    fresh: Option<&RepoWorktreeRow>,
) -> Result<(), String> {
    let Some(fresh) = fresh else {
        return Err("gone from git worktree list".to_string());
    };
    if fresh.verdict.state != RepoWorktreeState::Reclaimable {
        return Err(format!("now {}", fresh.verdict.reason));
    }
    if fresh.branch.is_none() || fresh.branch != cached.branch {
        return Err("branch changed".to_string());
    }
    match (cached.tip.as_deref(), fresh.tip.as_deref()) {
        (Some(was), Some(now)) if was == now => Ok(()),
        (None, _) | (_, None) => Err("tip unknown".to_string()),
        _ => Err("tip moved".to_string()),
    }
}

/// `git branch -D` runs only when the local branch still points at the tip
/// the verdict proved landed. `-D` is needed because a squash-merged tip is
/// never "merged" by ancestry (DD-122); this equality is what makes it safe.
pub(crate) fn branch_delete_decision(
    branch_tip_now: Option<&str>,
    verified_tip: &str,
) -> Result<(), &'static str> {
    match branch_tip_now {
        None => Err("local branch already gone"),
        Some(now) if now == verified_tip => Ok(()),
        Some(_) => Err("local branch moved"),
    }
}

/// Remote branch deletion: off unless the user opted in, and then only when
/// the remote-tracking ref is exactly the verified tip. The push itself also
/// carries `--force-with-lease=<ref>:<tip>`, so a remote that moved since the
/// last fetch refuses the delete.
pub(crate) fn remote_delete_decision(
    opted_in: bool,
    remote_tip: Option<&str>,
    verified_tip: &str,
) -> Result<(), &'static str> {
    if !opted_in {
        return Err("remote deletion off");
    }
    match remote_tip {
        None => Err("no remote branch"),
        Some(tip) if tip == verified_tip => Ok(()),
        Some(_) => Err("remote branch moved"),
    }
}

/// One snapshot of every visible process's cwd, the `ProcessFacts` way
/// (process-reaping.md): collected once, then consulted as data.
/// `available == false` means the table could not be read well enough to
/// trust a "nobody is inside" answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProcessCwdSnapshot {
    pub(crate) available: bool,
    pub(crate) cwds: Vec<PathBuf>,
}

/// Whether any process sits inside `worktree` (both canonical). `None` when
/// the snapshot is unavailable: doubt, which the verdict spares.
pub(crate) fn process_inside(snapshot: &ProcessCwdSnapshot, worktree: &Path) -> Option<bool> {
    if !snapshot.available {
        return None;
    }
    Some(snapshot.cwds.iter().any(|cwd| cwd.starts_with(worktree)))
}

/// PIDs of the daemon's own `git` helpers (its direct `git` children and
/// their `git` descendants), which the snapshot must ignore: the probe and
/// the pool run `git status` *inside* worktrees, and counting those would
/// make GC pin the very worktree it is inspecting. Anything else — a session
/// worker, a shell, an editor, a non-git child — still counts.
/// `procs` is `(pid, parent, image name)`.
pub(crate) fn own_git_helpers(
    procs: &[(u32, Option<u32>, String)],
    own_pid: u32,
) -> std::collections::HashSet<u32> {
    fn is_git(name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        name == "git" || name == "git.exe" || name.starts_with("git-")
    }
    let mut helpers = std::collections::HashSet::new();
    loop {
        let before = helpers.len();
        for (pid, parent, name) in procs {
            let Some(parent) = parent else { continue };
            if *pid != own_pid && is_git(name) && (*parent == own_pid || helpers.contains(parent)) {
                helpers.insert(*pid);
            }
        }
        if helpers.len() == before {
            return helpers;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::gc_service::repo_worktree::RepoWorktreeVerdict;
    use RepoWorktreeState::{Dangling, Pinned, Reclaimable};

    fn row(
        state: RepoWorktreeState,
        reason: &str,
        branch: Option<&str>,
        tip: Option<&str>,
    ) -> RepoWorktreeRow {
        RepoWorktreeRow {
            path: "/wt".to_string(),
            repo_root: "/repo".to_string(),
            branch: branch.map(str::to_string),
            tip: tip.map(str::to_string),
            mtime_unix: 0,
            verdict: RepoWorktreeVerdict {
                state,
                reason: reason.to_string(),
            },
        }
    }

    fn landed() -> RepoWorktreeRow {
        row(Reclaimable, "merged via PR #1", Some("feat"), Some("abc"))
    }

    // ---- mode ----

    #[test]
    fn mode_parses_default_off_observe_and_unknown() {
        assert_eq!(reclaim_mode_from_raw(None), ReclaimMode::Delete);
        assert_eq!(reclaim_mode_from_raw(Some("1")), ReclaimMode::Delete);
        for off in ["0", "false", "OFF", " no "] {
            assert_eq!(reclaim_mode_from_raw(Some(off)), ReclaimMode::Off, "{off}");
        }
        for observe in ["observe", "dry-run", "maybe"] {
            assert_eq!(
                reclaim_mode_from_raw(Some(observe)),
                ReclaimMode::Observe,
                "unknown values must not delete: {observe}"
            );
        }
    }

    // ---- selection: spare + reason first ----

    #[test]
    fn selection_spares_every_non_reclaimable_state() {
        for state in [Pinned, Dangling] {
            assert_eq!(
                reclaim_selection(ReclaimMode::Delete, state, false, false),
                ReclaimSelection::Spare("not reclaimable")
            );
        }
    }

    #[test]
    fn selection_spares_when_off() {
        assert_eq!(
            reclaim_selection(ReclaimMode::Off, Reclaimable, false, false),
            ReclaimSelection::Spare("repo-worktree gc off")
        );
    }

    #[test]
    fn selection_spares_a_path_with_a_live_session_now() {
        assert_eq!(
            reclaim_selection(ReclaimMode::Delete, Reclaimable, true, false),
            ReclaimSelection::Spare("process inside")
        );
    }

    #[test]
    fn selection_never_double_dispatches() {
        assert_eq!(
            reclaim_selection(ReclaimMode::Delete, Reclaimable, false, true),
            ReclaimSelection::Spare("reclaim already in flight")
        );
    }

    #[test]
    fn observe_mode_only_reports() {
        assert_eq!(
            reclaim_selection(ReclaimMode::Observe, Reclaimable, false, false),
            ReclaimSelection::WouldRemove
        );
    }

    #[test]
    fn delete_mode_dispatches_a_reclaimable_idle_row() {
        assert_eq!(
            reclaim_selection(ReclaimMode::Delete, Reclaimable, false, false),
            ReclaimSelection::Dispatch
        );
    }

    // ---- re-verification on the pool thread ----

    #[test]
    fn reverify_spares_a_worktree_that_vanished_from_the_list() {
        assert_eq!(
            reverify_reclaim(&landed(), None),
            Err("gone from git worktree list".to_string())
        );
    }

    #[test]
    fn reverify_spares_when_the_fresh_verdict_is_no_longer_reclaimable() {
        for reason in [
            "dirty",
            "untracked",
            "process inside",
            "open PR #4",
            "commits after merge",
            "unverifiable",
        ] {
            let fresh = row(Pinned, reason, Some("feat"), Some("abc"));
            assert_eq!(
                reverify_reclaim(&landed(), Some(&fresh)),
                Err(format!("now {reason}"))
            );
        }
    }

    #[test]
    fn reverify_spares_a_moved_tip_or_changed_branch() {
        let moved = row(Reclaimable, "merged via PR #1", Some("feat"), Some("def"));
        assert_eq!(
            reverify_reclaim(&landed(), Some(&moved)),
            Err("tip moved".to_string())
        );
        let renamed = row(Reclaimable, "merged via PR #1", Some("other"), Some("abc"));
        assert_eq!(
            reverify_reclaim(&landed(), Some(&renamed)),
            Err("branch changed".to_string())
        );
        let detached = row(Reclaimable, "merged via PR #1", None, Some("abc"));
        assert_eq!(
            reverify_reclaim(&landed(), Some(&detached)),
            Err("branch changed".to_string())
        );
    }

    #[test]
    fn reverify_spares_an_unknown_tip() {
        let unknown = row(Reclaimable, "merged via PR #1", Some("feat"), None);
        assert_eq!(
            reverify_reclaim(&landed(), Some(&unknown)),
            Err("tip unknown".to_string())
        );
        assert_eq!(
            reverify_reclaim(&unknown, Some(&landed())),
            Err("tip unknown".to_string())
        );
    }

    #[test]
    fn reverify_passes_an_unchanged_reclaimable_worktree() {
        assert_eq!(reverify_reclaim(&landed(), Some(&landed())), Ok(()));
    }

    // ---- ref deletions ----

    #[test]
    fn branch_delete_requires_the_verified_tip() {
        assert_eq!(
            branch_delete_decision(None, "abc"),
            Err("local branch already gone")
        );
        assert_eq!(
            branch_delete_decision(Some("def"), "abc"),
            Err("local branch moved")
        );
        assert_eq!(branch_delete_decision(Some("abc"), "abc"), Ok(()));
    }

    #[test]
    fn remote_delete_is_off_by_default_and_needs_an_exact_tip() {
        assert_eq!(
            remote_delete_decision(false, Some("abc"), "abc"),
            Err("remote deletion off")
        );
        assert_eq!(
            remote_delete_decision(true, None, "abc"),
            Err("no remote branch")
        );
        assert_eq!(
            remote_delete_decision(true, Some("def"), "abc"),
            Err("remote branch moved")
        );
        assert_eq!(remote_delete_decision(true, Some("abc"), "abc"), Ok(()));
    }

    // ---- process table ----

    #[test]
    fn only_the_daemons_own_git_helpers_are_ignored() {
        let own = 100;
        let procs = vec![
            (1, Some(own), "git".to_string()),
            (2, Some(1), "git-remote-https".to_string()),
            (3, Some(own), "claude".to_string()),
            (4, Some(3), "git".to_string()),
            (5, Some(999), "git.exe".to_string()),
            (6, None, "git".to_string()),
        ];
        let helpers = own_git_helpers(&procs, own);
        assert_eq!(helpers, [1, 2].into_iter().collect());
    }

    #[test]
    fn an_unreadable_process_table_is_doubt() {
        let snapshot = ProcessCwdSnapshot {
            available: false,
            cwds: vec![],
        };
        assert_eq!(process_inside(&snapshot, Path::new("/wt")), None);
    }

    #[test]
    fn a_process_cwd_inside_or_at_the_worktree_pins_it() {
        let snapshot = ProcessCwdSnapshot {
            available: true,
            cwds: vec![PathBuf::from("/wt/src"), PathBuf::from("/elsewhere")],
        };
        assert_eq!(process_inside(&snapshot, Path::new("/wt")), Some(true));
        assert_eq!(
            process_inside(&snapshot, Path::new("/elsewhere")),
            Some(true)
        );
        assert_eq!(process_inside(&snapshot, Path::new("/wt2")), Some(false));
        // Component-wise, not string prefix: `/wt` does not contain `/wtx`.
        let sibling = ProcessCwdSnapshot {
            available: true,
            cwds: vec![PathBuf::from("/wtx")],
        };
        assert_eq!(process_inside(&sibling, Path::new("/wt")), Some(false));
    }
}
