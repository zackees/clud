//! Issue #1603: the side-effecting half of repo-worktree reclaim. Runs only
//! on a purge-pool thread (`clud-gc-purge-N`), never on the registry worker
//! (#946). Every decision it acts on is a pure function in
//! `repo_worktree_reclaim`; this file only gathers fresh facts and runs git
//! through `running-process` (`extern_repo::probe_cmd`).
//!
//! Order, each step gated on the one before:
//! 1. Re-probe the one worktree from scratch (fresh `git`, fresh PR lookup,
//!    fresh process table) and require the verdict to be unchanged
//!    ([`reverify_reclaim`]). Any veto spares and is logged.
//! 2. `git worktree remove <path>` — never `--force`. git itself refuses a
//!    worktree that became dirty or untracked in the last instant, which is
//!    a second, independent guard. Read-only entries are made writable first
//!    so a clean tree is not half-deleted by a permission error.
//! 3. `git branch -D <branch>` only if the branch still points at the
//!    verified tip ([`branch_delete_decision`]).
//! 4. Opt-in only: delete `origin/<branch>` with a lease on the verified tip
//!    ([`remote_delete_decision`]).
//! 5. `git worktree prune`.
//!
//! Issue #1632: the whole sequence runs under a per-repo lock
//! ([`run_reclaim_serialized`]), so two pool threads never run git against
//! the same repository at once. Different repos still run in parallel.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use super::extern_repo::{git_discovery_env_is_poisoned, probe_cmd_streams};
use super::repo_worktree_probe::{
    collect_process_cwds, probe_one, probe_reservation, PrRecord, RepoWorktreeRow,
};
use super::repo_worktree_reclaim::{
    branch_delete_decision, remote_delete_decision, reverify_reclaim, reverify_reservation,
};
use crate::gc::worktree_root::is_under_worktree_root;

const GIT_QUERY_TIMEOUT: Duration = Duration::from_secs(10);
/// Removing a worktree deletes its ignored build output too (a `target/` can
/// be many GB). Killing git mid-delete would leave a half-removed tree that
/// then reads as dirty forever, so the bound is generous.
const GIT_REMOVE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const GIT_PUSH_TIMEOUT: Duration = Duration::from_secs(60);

/// One reclaim handed to the purge pool.
#[derive(Debug, Clone)]
pub(crate) struct ReclaimJob {
    /// The cached row that authorized dispatch.
    pub(crate) row: RepoWorktreeRow,
    /// `gc.delete_remote_branches` / `CLUD_GC_DELETE_REMOTE_BRANCHES`.
    pub(crate) delete_remote: bool,
    /// Live clud session cwds at dispatch time. The fresh process table
    /// covers them too; these are belt and braces.
    pub(crate) session_cwds: Vec<PathBuf>,
    /// #1485: the worktree root the verdict was computed against, so the
    /// re-check applies the same abandoned-empty scope.
    pub(crate) wt_root: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReclaimOutcome {
    /// The worktree directory is gone. `notes` says what happened to the
    /// branch, the remote branch and the prune.
    Removed { notes: Vec<String> },
    /// Vetoed before anything was touched.
    Spared(String),
    /// A git step failed; nothing past it ran.
    Failed(String),
}

/// Issue #1632: one mutex per repository, so reclaims of two worktrees of
/// the same repo run one after the other while different repos stay
/// parallel. `git worktree remove`, `branch -D` and `worktree prune` all
/// rewrite shared state under the repo's git dir (`worktrees/`, refs, their
/// `.lock` files); run concurrently they can fail with exit 255, which on
/// Windows' mandatory file locks happened often enough to flake CI.
///
/// Deadlock-free by construction: a thread holds at most one repo lock,
/// and the map's own mutex is held only to look up or drop an entry, never
/// while waiting for a repo lock or running git. The lock is taken on the
/// pool thread after the job has left the queue, so it never stalls
/// dispatch. An entry is dropped once nothing references it, so the map
/// holds only repos with a reclaim in flight.
#[derive(Default)]
pub(crate) struct RepoLocks {
    map: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl RepoLocks {
    /// Run `f` while holding the lock for `key`.
    pub(crate) fn with_lock<T>(&self, key: &Path, f: impl FnOnce() -> T) -> T {
        let _ = (key, &self.map);
        f()
    }
}

/// The daemon-wide lock map every pool thread shares.
static RECLAIM_LOCKS: LazyLock<RepoLocks> = LazyLock::new(RepoLocks::default);

/// The lock key for a repo root: canonical when it resolves, so two
/// spellings of one repo share a lock; the raw path otherwise.
fn repo_lock_key(repo_root: &str) -> PathBuf {
    let raw = PathBuf::from(repo_root);
    std::fs::canonicalize(&raw).unwrap_or(raw)
}

/// [`run_reclaim`] under the per-repo lock (#1632). Reservation rows have no
/// repo and never run git, so they skip the lock.
pub(crate) fn run_reclaim_serialized(
    job: &ReclaimJob,
    lookup_prs: &dyn Fn(&Path) -> Option<Vec<PrRecord>>,
) -> ReclaimOutcome {
    serialize_by_repo(&RECLAIM_LOCKS, job, || run_reclaim(job, lookup_prs))
}

fn serialize_by_repo<T>(locks: &RepoLocks, job: &ReclaimJob, f: impl FnOnce() -> T) -> T {
    if job.row.reservation || job.row.repo_root.is_empty() {
        return f();
    }
    locks.with_lock(&repo_lock_key(&job.row.repo_root), f)
}

fn git(cwd: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    match probe_cmd_streams("git", cwd, args, timeout) {
        Some((0, out, _)) => Ok(out),
        Some((code, _, err)) => {
            let err = err.trim();
            if err.is_empty() {
                Err(format!("git {} exited {code}", args.join(" ")))
            } else {
                Err(format!("git {} exited {code}: {err}", args.join(" ")))
            }
        }
        None => Err(format!(
            "git {} timed out or failed to start",
            args.join(" ")
        )),
    }
}

fn rev(cwd: &Path, refname: &str) -> Option<String> {
    git(
        cwd,
        &["rev-parse", "--verify", "--quiet", refname],
        GIT_QUERY_TIMEOUT,
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

/// Make every directory (and, on Windows, every file) under `root` writable,
/// without following symlinks, so `git worktree remove` does not stop half
/// way on a permission error. Git does not track the write bit, so this
/// cannot make a clean tree dirty. Best effort: a failure here just lets git
/// report the real error.
fn clear_readonly(root: &Path) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        make_writable(&path, &meta);
        if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        }
    }
}

#[cfg(unix)]
fn make_writable(path: &Path, meta: &std::fs::Metadata) {
    use std::os::unix::fs::PermissionsExt;
    // Unlinking needs write on the parent directory only; files are fine.
    if !meta.is_dir() {
        return;
    }
    let mode = meta.permissions().mode();
    if mode & 0o700 != 0o700 {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o700));
    }
}

#[cfg(windows)]
fn make_writable(path: &Path, meta: &std::fs::Metadata) {
    let mut perms = meta.permissions();
    if perms.readonly() {
        // Windows has no mode bits: this only clears FILE_ATTRIBUTE_READONLY.
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(any(unix, windows)))]
fn make_writable(_path: &Path, _meta: &std::fs::Metadata) {}

/// Run one reclaim. `lookup_prs` is injected so tests never reach the
/// network; production passes `repo_worktree_probe::lookup_prs`.
pub(crate) fn run_reclaim(
    job: &ReclaimJob,
    lookup_prs: &dyn Fn(&Path) -> Option<Vec<PrRecord>>,
) -> ReclaimOutcome {
    if job.row.reservation {
        return run_reservation_reclaim(job);
    }
    if git_discovery_env_is_poisoned() {
        return ReclaimOutcome::Spared("git discovery env set".to_string());
    }
    let repo_root = PathBuf::from(&job.row.repo_root);
    let worktree = PathBuf::from(&job.row.path);

    // 1. Fresh facts, then the pure re-check.
    let procs = collect_process_cwds();
    let prs = lookup_prs(&repo_root);
    let fresh = probe_one(
        &repo_root,
        &worktree,
        &job.session_cwds,
        &procs,
        prs.as_deref(),
        job.wt_root.as_deref(),
    );
    if let Err(reason) = reverify_reclaim(&job.row, fresh.as_ref()) {
        return ReclaimOutcome::Spared(reason);
    }
    let Some(fresh) = fresh else {
        return ReclaimOutcome::Spared("gone from git worktree list".to_string());
    };
    let (Some(branch), Some(tip)) = (fresh.branch.clone(), fresh.tip.clone()) else {
        return ReclaimOutcome::Spared("tip unknown".to_string());
    };

    // 2. Remove the worktree. Never --force.
    clear_readonly(&worktree);
    let path_arg = worktree.to_string_lossy().to_string();
    if let Err(err) = git(
        &repo_root,
        &["worktree", "remove", &path_arg],
        GIT_REMOVE_TIMEOUT,
    ) {
        return ReclaimOutcome::Failed(err);
    }
    if worktree.try_exists().unwrap_or(true) {
        return ReclaimOutcome::Failed("directory still present after git worktree remove".into());
    }

    let mut notes = Vec::new();

    // 3. Local branch, only at the verified tip.
    let local_ref = format!("refs/heads/{branch}");
    match branch_delete_decision(rev(&repo_root, &local_ref).as_deref(), &tip) {
        Ok(()) => match git(&repo_root, &["branch", "-D", &branch], GIT_QUERY_TIMEOUT) {
            Ok(_) => notes.push(format!("deleted branch {branch}")),
            Err(err) => notes.push(format!("kept branch {branch}: {err}")),
        },
        Err(reason) => notes.push(format!("kept branch {branch}: {reason}")),
    }

    // 4. Remote branch: opt-in, exact tip, leased.
    let remote_tip = rev(&repo_root, &format!("refs/remotes/origin/{branch}"));
    match remote_delete_decision(job.delete_remote, remote_tip.as_deref(), &tip) {
        Ok(()) => {
            let lease = format!("--force-with-lease={local_ref}:{tip}");
            let refspec = format!(":{local_ref}");
            match git(
                &repo_root,
                &["push", &lease, "origin", &refspec],
                GIT_PUSH_TIMEOUT,
            ) {
                Ok(_) => notes.push(format!("deleted origin/{branch}")),
                Err(err) => notes.push(format!("kept origin/{branch}: {err}")),
            }
        }
        Err(reason) => notes.push(format!("kept origin/{branch}: {reason}")),
    }

    // 5. Prune stale administrative entries.
    if let Err(err) = git(&repo_root, &["worktree", "prune"], GIT_QUERY_TIMEOUT) {
        notes.push(format!("prune failed: {err}"));
    }
    ReclaimOutcome::Removed { notes }
}

/// Issue #1486: reclaim an unused `tmp-wt` reservation. Re-probes it from
/// scratch (fresh process table, fresh emptiness, fresh age), requires the
/// unchanged `reserved-unused` verdict ([`reverify_reservation`]), then
/// calls `remove_dir`, which the OS refuses for a directory that gained an
/// entry in the last instant. Never recursive, never git.
fn run_reservation_reclaim(job: &ReclaimJob) -> ReclaimOutcome {
    let path = PathBuf::from(&job.row.path);
    let still_under_root = job
        .wt_root
        .as_deref()
        .is_some_and(|root| is_under_worktree_root(&path, root));
    let procs = collect_process_cwds();
    let fresh = probe_reservation(&path, &job.session_cwds, &procs);
    if let Err(reason) = reverify_reservation(&fresh, still_under_root) {
        return ReclaimOutcome::Spared(reason);
    }
    match std::fs::remove_dir(&path) {
        Ok(()) => ReclaimOutcome::Removed {
            notes: vec!["removed unused reservation".to_string()],
        },
        Err(err) => ReclaimOutcome::Failed(format!("remove_dir: {err}")),
    }
}

#[cfg(test)]
#[path = "repo_worktree_reclaim_exec_tests.rs"]
mod tests;
