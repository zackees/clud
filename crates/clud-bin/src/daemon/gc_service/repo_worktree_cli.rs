//! Issue #1606: the narrow bridge `clud --clean-worktrees`
//! (`crate::worktrees`) uses to share the daemon's squash-aware verdict
//! (`repo_worktree_verdict`), its fact gathering (`repo_worktree_probe`) and
//! its reclaim executor (`repo_worktree_reclaim_exec`), so the CLI and the
//! daemon agree on what landed and remove it the same way.
//!
//! Nothing here decides anything: the decision stays the pure verdict, and
//! the CLI's own precedence lives in `worktrees_verdict.rs`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

pub(crate) use super::repo_worktree::{RepoWorktreeState, RepoWorktreeVerdict};
pub(crate) use super::repo_worktree_probe::{PrRecord, RepoWorktreeRow};
pub(crate) use super::repo_worktree_reclaim::ProcessCwdSnapshot;
pub(crate) use super::repo_worktree_reclaim_exec::ReclaimOutcome;

use super::repo_worktree_probe::{collect_process_cwds, lookup_prs_with_timeout, probe_repo_each};
use super::repo_worktree_reclaim_exec::{run_reclaim_with, ReclaimJob};

/// Issue #1648: `gh` timeout for the interactive CLI. The daemon's 20 s is
/// fine on a background tick; a person (or a CI smoke test) is waiting here.
const CLI_GH_TIMEOUT: Duration = Duration::from_secs(3);

/// Injected PR lookup, shareable with the verdict worker thread.
pub(crate) type PrLookup = Arc<dyn Fn(&Path) -> Option<Vec<PrRecord>> + Send + Sync>;
/// Injected process-table snapshot, shareable with the verdict worker thread.
pub(crate) type ProcsSource = Arc<dyn Fn() -> ProcessCwdSnapshot + Send + Sync>;

/// The live process table, as the daemon reads it.
pub(crate) fn live_process_cwds() -> ProcessCwdSnapshot {
    collect_process_cwds()
}

/// The repo's PRs for the interactive CLI, with a short `gh` timeout.
pub(crate) fn lookup_prs_for_cli(main_repo: &Path) -> Option<Vec<PrRecord>> {
    lookup_prs_with_timeout(main_repo, CLI_GH_TIMEOUT)
}

/// Issue #1648: gather verdict rows for every worktree of `main_repo` on a
/// worker thread and stream them back, so the CLI can stop waiting at its
/// deadline and keep whatever arrived. The CLI has no session registry, so
/// it relies on the process table alone for "process inside".
///
/// Cost is bounded per run, not per worktree: one process-table snapshot,
/// at most one `gh pr list` (and only if some worktree reaches the PR
/// check), and the per-worktree git probes run one at a time. The channel
/// disconnects when the probe is complete; a worker still running when the
/// CLI stops waiting is abandoned (it only reads, and dies with the process).
pub(crate) fn spawn_cli_probe(
    main_repo: PathBuf,
    lookup: PrLookup,
    procs: ProcsSource,
    wt_root: Option<PathBuf>,
) -> Receiver<RepoWorktreeRow> {
    let (tx, rx) = mpsc::channel();
    // A thread that cannot start drops `tx`, yielding no rows; the caller
    // then treats every worktree as having no verdict (spare on doubt).
    let _ = std::thread::Builder::new()
        .name("clean-worktrees-verdict".into())
        .spawn(move || {
            let snapshot = procs();
            let prs: OnceLock<Option<Vec<PrRecord>>> = OnceLock::new();
            let lazy_prs = || prs.get_or_init(|| lookup(&main_repo)).as_deref();
            probe_repo_each(
                &main_repo,
                &snapshot,
                &lazy_prs,
                wt_root.as_deref(),
                &mut |row| {
                    let _ = tx.send(row);
                },
            );
        });
    rx
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
