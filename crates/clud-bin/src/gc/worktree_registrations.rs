//! Dead git worktree registrations (#2397).
//!
//! A repo's `<common-dir>/worktrees/<name>/` administrative entry outlives the
//! worktree directory when that directory is deleted by hand, by a container,
//! or by a crashed session. `git worktree list` keeps printing it and
//! `git worktree remove` on it is a no-op, so on a busy box hundreds of them
//! pile up across repos.
//!
//! This module counts such entries with plain filesystem reads. An entry is
//! dead only when the `.git` file named in its `gitdir` file is gone, the
//! entry is not `locked`, **and the worktree directory's parent exists**. A
//! missing parent (an unmounted drive, a detached volume, a container path)
//! proves nothing about the worktree, so such an entry is "unreachable" and
//! neither counted as dead nor pruned; a repo holding any unreachable entry is
//! not pruned at all, because git cannot be told to skip single entries.
//! Pruning is plain `git worktree prune` (no `--expire`), run through
//! `worktrees::run_git` (running-process). Entries that are still dead after
//! it are reported as "dead, pending git expiry". Pruning is metadata only: it
//! never touches a worktree directory, so it needs no liveness guard.
//!
//! The repos come from the registry's `repo_visits` table, the same list `clud
//! ui` shows; a repo clud has never run in is not visited.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::worktrees;

/// Per-repo outcome of [`prune_repos`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoOutcome {
    pub repo: String,
    /// Dead registrations found before pruning.
    pub dead: usize,
    /// Registrations that are gone after pruning (always 0 in a dry run).
    pub pruned: usize,
    /// Dead registrations that survived `git worktree prune` (git's own
    /// expiry); always 0 in a dry run.
    pub pending: usize,
    /// Entries whose worktree parent directory is missing (unmounted volume?):
    /// not counted dead, and their repo is not pruned.
    pub unreachable: usize,
    /// Why this repo was skipped or the prune failed.
    pub error: Option<String>,
}

/// Aggregate outcome across repos.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    pub repos: Vec<RepoOutcome>,
    pub dry_run: bool,
}

impl Report {
    pub fn total_dead(&self) -> usize {
        self.repos.iter().map(|r| r.dead).sum()
    }

    pub fn total_pruned(&self) -> usize {
        self.repos.iter().map(|r| r.pruned).sum()
    }

    pub fn total_pending(&self) -> usize {
        self.repos.iter().map(|r| r.pending).sum()
    }

    pub fn total_unreachable(&self) -> usize {
        self.repos.iter().map(|r| r.unreachable).sum()
    }

    /// The one line `clud gc prune` prints.
    pub fn summary_line(&self) -> String {
        let repos = self.repos.iter().filter(|r| r.dead > 0).count();
        let mut line = if self.dry_run {
            format!(
                "worktree-registrations: --dry-run would prune {} dead registration(s) in {repos} repo(s)",
                self.total_dead()
            )
        } else {
            format!(
                "worktree-registrations: pruned {} dead registration(s) in {repos} repo(s)",
                self.total_pruned()
            )
        };
        if self.total_pending() > 0 {
            line.push_str(&format!(
                "; {} dead, pending git expiry",
                self.total_pending()
            ));
        }
        if self.total_unreachable() > 0 {
            line.push_str(&format!(
                "; {} unreachable (parent directory missing), skipped",
                self.total_unreachable()
            ));
        }
        line.push('.');
        line
    }
}

/// The repo's shared git directory (`.git`, or the bare repo itself).
fn common_dir(repo: &Path) -> Result<PathBuf, String> {
    let out = worktrees::run_git(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    Ok(PathBuf::from(out.trim()))
}

/// Registrations classified by [`classify_entries`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Classified {
    /// Worktree gone, parent directory present, not locked.
    pub dead: Vec<PathBuf>,
    /// Worktree gone and its parent directory missing too: unknown, spared.
    pub unreachable: Vec<PathBuf>,
}

/// Classify the administrative entries under `common/worktrees`. A `locked`
/// entry is spared, as `git worktree prune` spares it.
pub fn classify_entries(common: &Path) -> Classified {
    let Ok(entries) = fs::read_dir(common.join("worktrees")) else {
        return Classified::default();
    };
    let mut out = Classified::default();
    for entry in entries.flatten() {
        let admin = entry.path();
        if !admin.is_dir() || admin.join("locked").exists() {
            continue;
        }
        let target = fs::read_to_string(admin.join("gitdir"))
            .ok()
            .map(|raw| PathBuf::from(raw.trim()))
            .filter(|path| !path.as_os_str().is_empty());
        // A missing or empty `gitdir` file is prunable too ("gitdir file does
        // not exist" in git's own report).
        let Some(target) = target else {
            out.dead.push(admin);
            continue;
        };
        if target.exists() {
            continue;
        }
        // `target` is `<worktree>/.git`; the worktree's parent must be there
        // and non-empty. An empty parent is most likely an unmounted mount
        // point, so it proves nothing about the worktree.
        let parent_present = target
            .parent()
            .and_then(Path::parent)
            .and_then(|p| fs::read_dir(p).ok())
            .is_some_and(|mut entries| entries.next().is_some());
        if parent_present {
            out.dead.push(admin);
        } else {
            out.unreachable.push(admin);
        }
    }
    out.dead.sort();
    out.unreachable.sort();
    out
}

/// Dead entries only (see [`classify_entries`]).
pub fn dead_entries(common: &Path) -> Vec<PathBuf> {
    classify_entries(common).dead
}

fn prune_one(repo: &str, common: &Path, dry_run: bool) -> RepoOutcome {
    let found = classify_entries(common);
    let dead = found.dead.len();
    let mut outcome = RepoOutcome {
        repo: repo.to_string(),
        dead,
        pruned: 0,
        pending: 0,
        unreachable: found.unreachable.len(),
        error: None,
    };
    if dry_run || dead == 0 {
        return outcome;
    }
    if outcome.unreachable > 0 {
        // Plain `git worktree prune` would take the unreachable entries too.
        outcome.error = Some("skipped: unreachable registrations present".to_string());
        return outcome;
    }
    match worktrees::run_git(Path::new(repo), &["worktree", "prune"]) {
        Ok(_) => {
            outcome.pending = dead_entries(common).len();
            outcome.pruned = dead.saturating_sub(outcome.pending);
        }
        Err(err) => outcome.error = Some(err),
    }
    outcome
}

/// Count (and, unless `dry_run`, prune) dead registrations in each repo.
/// Two roots sharing a common dir (a repo and one of its worktrees) are
/// handled once.
pub fn prune_repos(repos: &[String], dry_run: bool) -> Report {
    let mut seen = BTreeSet::new();
    let mut report = Report {
        repos: Vec::new(),
        dry_run,
    };
    for repo in repos {
        if !Path::new(repo).is_dir() {
            continue;
        }
        let common = match common_dir(Path::new(repo)) {
            Ok(common) => common,
            Err(error) => {
                report.repos.push(RepoOutcome {
                    repo: repo.clone(),
                    dead: 0,
                    pruned: 0,
                    pending: 0,
                    unreachable: 0,
                    error: Some(error),
                });
                continue;
            }
        };
        if seen.insert(common.clone()) {
            report.repos.push(prune_one(repo, &common, dry_run));
        }
    }
    report
}

/// Dead registrations across `repos`, without changing anything.
pub fn count_dead(repos: &[String]) -> usize {
    prune_repos(repos, true).total_dead()
}

#[cfg(test)]
#[path = "worktree_registrations_tests.rs"]
mod tests;
