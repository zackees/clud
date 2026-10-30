//! Dead git worktree registrations (#2397).
//!
//! A repo's `<common-dir>/worktrees/<name>/` administrative entry outlives the
//! worktree directory when that directory is deleted by hand, by a container,
//! or by a crashed session. `git worktree list` keeps printing it and
//! `git worktree remove` on it is a no-op, so on a busy box hundreds of them
//! pile up across repos.
//!
//! This module counts such entries with plain filesystem reads (an entry is
//! dead when the path in its `gitdir` file no longer exists and the entry is
//! not `locked`, the same test `git worktree prune` applies) and prunes them
//! with `git worktree prune --expire=now`, run through `worktrees::run_git`
//! (running-process). Pruning is metadata only: it never touches a worktree
//! directory, so it needs no liveness guard.
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

    /// The one line `clud gc prune` prints.
    pub fn summary_line(&self) -> String {
        let repos = self.repos.iter().filter(|r| r.dead > 0).count();
        if self.dry_run {
            format!(
                "worktree-registrations: --dry-run would prune {} dead registration(s) in {repos} repo(s).",
                self.total_dead()
            )
        } else {
            format!(
                "worktree-registrations: pruned {} dead registration(s) in {repos} repo(s).",
                self.total_pruned()
            )
        }
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

/// Administrative entries under `common/worktrees` whose worktree directory is
/// gone. A `locked` entry is spared, as `git worktree prune` spares it.
pub fn dead_entries(common: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(common.join("worktrees")) else {
        return Vec::new();
    };
    let mut dead = Vec::new();
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
        if target.is_none_or(|path| !path.exists()) {
            dead.push(admin);
        }
    }
    dead.sort();
    dead
}

fn prune_one(repo: &str, common: &Path, dry_run: bool) -> RepoOutcome {
    let dead = dead_entries(common).len();
    let mut outcome = RepoOutcome {
        repo: repo.to_string(),
        dead,
        pruned: 0,
        error: None,
    };
    if dry_run || dead == 0 {
        return outcome;
    }
    match worktrees::run_git(Path::new(repo), &["worktree", "prune", "--expire=now"]) {
        Ok(_) => outcome.pruned = dead.saturating_sub(dead_entries(common).len()),
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
