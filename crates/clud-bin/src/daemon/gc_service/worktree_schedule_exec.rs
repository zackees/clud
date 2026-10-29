//! Issue #2101 item 1: the probe-thread half of warn-then-clean. Gathers the
//! facts for `gc::worktree_schedule::plan`, persists the schedule, and moves
//! due worktrees to `clud trash` (registering the trash row with the
//! registry worker so the normal trash sweep reclaims it after 72 h).

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use crate::gc::keep_list;
use crate::gc::target_prune;
use crate::gc::worktree_root::SizeCheck;
use crate::gc::worktree_schedule::{self, Action, Candidate};
use crate::gc::worktree_size_cache::{cache_path_for, now_unix, read_cache};

use super::repo_worktree::RepoWorktreeState;
use super::repo_worktree_probe::RepoWorktreeRow;
use super::repo_worktree_reclaim::{ProcessCwdSnapshot, ReclaimMode};
use super::{GcOp, GcRequestMsg, RegistryMsg};

const INSERT_REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// Kept, reserved, reclaimable-elsewhere, or holding a live session/process.
fn in_use(
    r: &RepoWorktreeRow,
    keep: &keep_list::KeepList,
    live_cwds: &[PathBuf],
    procs: &ProcessCwdSnapshot,
) -> bool {
    r.reservation
        || r.verdict.state == RepoWorktreeState::Reclaimable
        || keep.keeps(&r.path)
        || super::entry_path_contains_live_cwd_path(&r.path, live_cwds)
        || super::source_guard::source_in_use_with_processes(Path::new(&r.path), procs).is_some()
}

/// Build one candidate per large worktree. Anything the probe did not see as
/// a worktree, a reservation, a reclaimable row, a kept path or a path with a
/// live session or process inside is exempt.
fn candidates(
    wt_root: &Path,
    rows: &[RepoWorktreeRow],
    live_cwds: &[PathBuf],
    procs: &ProcessCwdSnapshot,
    now: i64,
) -> Vec<Candidate> {
    let keep = keep_list::load(&keep_list::keep_path_for(wt_root));
    let top = crate::gc::worktree_top::read_for(wt_root, now, 6 * 60 * 60);
    top.into_iter()
        .map(|t| {
            let row = rows.iter().find(|r| r.path == t.path);
            let exempt = row.is_none_or(|r| in_use(r, &keep, live_cwds, procs));
            Candidate {
                path: t.path,
                bytes: t.bytes,
                idle_secs: row.map_or(0, |r| now.saturating_sub(r.mtime_unix)),
                exempt,
            }
        })
        .collect()
}

/// One tick of warn-then-clean. Production only; without a process snapshot
/// nothing shows which worktrees are in use, so nothing is scheduled.
pub(super) fn run(
    wt_root: Option<&Path>,
    rows: &[RepoWorktreeRow],
    live_cwds: &[PathBuf],
    procs: &ProcessCwdSnapshot,
    mode: ReclaimMode,
    tx: &mpsc::Sender<RegistryMsg>,
) {
    let Some(wt_root) = wt_root else { return };
    if cfg!(test) || !procs.available || mode == ReclaimMode::Off {
        return;
    }
    let now = now_unix();
    let cached = read_cache(&cache_path_for(wt_root));
    let mut over = matches!(cached.map(|c| c.check), Some(SizeCheck::Over(_)));
    let mut schedule = worktree_schedule::load(wt_root);
    let mut cands = candidates(wt_root, rows, live_cwds, procs, now);
    if over && mode == ReclaimMode::Delete {
        let warn_bytes = cached.map_or(0, |c| c.warn_bytes);
        over = prune_build_output(wt_root, rows, live_cwds, &mut cands, warn_bytes, now);
    }
    let actions = worktree_schedule::plan(&mut schedule, over, &cands, now);
    let _ = worktree_schedule::save(wt_root, &schedule);
    for action in actions {
        match action {
            Action::Warn {
                path,
                bytes,
                remove_after,
            } => eprintln!(
                "[clud] gc: {path} ({bytes} bytes) is unmerged and over worktrees.warn_bytes; \
                 it moves to clud trash after unix {remove_after} unless `clud gc keep` pins it"
            ),
            Action::Trash { path } if mode == ReclaimMode::Observe => {
                eprintln!("[clud] gc tick: observe mode, would trash oversized worktree {path}");
            }
            Action::Trash { path } => trash(&path, tx, now),
        }
    }
}

/// #2302: prune `target/` dirs of idle, unused worktrees before anything is
/// scheduled for trash. Returns whether the root is still over budget; any
/// failure (no soldr, prune error) leaves the previous verdict standing.
fn prune_build_output(
    wt_root: &Path,
    rows: &[RepoWorktreeRow],
    live_cwds: &[PathBuf],
    cands: &mut [Candidate],
    warn_bytes: u64,
    now: i64,
) -> bool {
    let picked: Vec<usize> = (0..cands.len())
        .filter(|&i| target_prune::eligible(&cands[i]))
        .collect();
    if picked.is_empty() {
        return true;
    }
    let mut run = match target_prune::soldr_runner() {
        Ok(r) => r,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("[clud] gc: build-output prune skipped: {e}");
            }
            return true;
        }
    };
    let mut state = target_prune::PruneState::load(wt_root);
    let mut total = 0u64;
    for i in picked {
        // Re-evaluated right before each prune with a fresh process table:
        // the snapshot behind `cands` is stale after earlier, slow prunes.
        let path = cands[i].path.clone();
        let unused = || {
            let keep = keep_list::load(&keep_list::keep_path_for(wt_root));
            let procs = super::repo_worktree_cli::live_process_cwds();
            procs.available
                && rows
                    .iter()
                    .find(|r| r.path == path)
                    .is_some_and(|r| !in_use(r, &keep, live_cwds, &procs))
        };
        let out = target_prune::prune_worktree(
            Path::new(&path),
            &mut run,
            &target_prune::real_size,
            &unused,
            &mut state,
            now,
        );
        if out.targets_pruned > 0 {
            eprintln!(
                "[clud] gc: pruned {} target dir(s) in {}, reclaimed {} bytes",
                out.targets_pruned, cands[i].path, out.reclaimed_bytes
            );
        }
        cands[i].bytes = cands[i].bytes.saturating_sub(out.reclaimed_bytes);
        total = total.saturating_add(out.reclaimed_bytes);
    }
    state.save(wt_root);
    if total == 0 {
        return true;
    }
    // Re-measure and record: the size report and top list now show the
    // post-prune magnitudes, not the stale pre-prune ones.
    let _ = crate::gc::worktree_size_cache::refresh_cache(wt_root, warn_bytes, now);
    matches!(
        read_cache(&cache_path_for(wt_root)).map(|c| c.check),
        Some(SizeCheck::Over(_))
    )
}

fn trash(path: &str, tx: &mpsc::Sender<RegistryMsg>, now: i64) {
    let trash_root = match crate::daemon::default_trash_dir() {
        Ok(p) => p,
        Err(e) => return eprintln!("[clud] gc: trash dir unavailable, kept {path}: {e}"),
    };
    let record = match crate::trash::trash_one_path(Path::new(path), &trash_root, false) {
        Ok(r) => r,
        Err(e) => return eprintln!("[clud] gc: could not trash {path}: {e}"),
    };
    eprintln!(
        "[clud] gc: moved oversized unmerged worktree {path} to {} (recoverable for 72 h)",
        record.quarantined_path.display()
    );
    let input = crate::trash::insert_input_for_record(&record, now);
    let op = GcOp::Insert {
        kind: input.kind,
        path: input.path,
        repo_root: input.repo_root,
        branch: input.branch,
        agent_id: input.agent_id,
        created_unix: Some(input.now_unix),
    };
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    if tx
        .send(RegistryMsg::Op(GcRequestMsg { op, reply_tx }))
        .is_err()
        || reply_rx.recv_timeout(INSERT_REPLY_TIMEOUT).is_err()
    {
        eprintln!(
            "[clud] gc: trash row for {} not registered; remove it by hand when no longer needed",
            record.quarantine_dir.display()
        );
    }
}
