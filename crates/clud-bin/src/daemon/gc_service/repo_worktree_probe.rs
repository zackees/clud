//! Issue #1591: gather [`RepoWorktreeFacts`] for every worktree of the repos
//! the GC registry knows about, and turn them into verdicts.
//!
//! Runs only on the `clud-gc-repo-worktree-probe` thread, never on the
//! registry worker (#946): every query is a bounded `git`/`gh` spawn through
//! `running-process` (`extern_repo::probe_cmd`). A query that fails or times
//! out yields `None`/`Unavailable`, which the verdict treats as doubt and
//! spares. Read-only: nothing here mutates a repository.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde::Deserialize;

use crate::gc::extract_pid_from_lock_reason;
use crate::session_registry::{LivenessProbe, OsLivenessProbe};
use crate::worktrees::{parse_worktree_porcelain, WorktreeEntry};

use super::extern_repo::{git_discovery_env_is_poisoned, probe_cmd};
use super::repo_worktree::{repo_worktree_verdict, PrFact, RepoWorktreeFacts, RepoWorktreeVerdict};

/// `clud gc list` kind for these rows. Not a registry kind: no redb row
/// backs them, so no purge path can select them.
pub(crate) const REPO_WORKTREE_KIND: &str = "repo-worktree";

const GIT_TIMEOUT: Duration = Duration::from_secs(5);
/// `gh` talks to the network; give it longer, still bounded.
const GH_TIMEOUT: Duration = Duration::from_secs(20);
/// Cap on default-branch commits scanned for a patch match per worktree.
const PATCH_SCAN_LIMIT: &str = "100";

/// One probed worktree, as cached for `clud gc list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoWorktreeRow {
    pub(crate) path: String,
    pub(crate) repo_root: String,
    pub(crate) branch: Option<String>,
    /// Directory mtime, standing in for a creation time in `gc list`.
    pub(crate) mtime_unix: i64,
    pub(crate) verdict: RepoWorktreeVerdict,
}

/// One row of `gh pr list --json number,state,headRefName,headRefOid`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct PrRecord {
    pub(crate) number: u64,
    pub(crate) state: String,
    #[serde(rename = "headRefName")]
    pub(crate) head_ref_name: String,
    #[serde(rename = "headRefOid")]
    pub(crate) head_ref_oid: String,
}

/// Pure: fold every PR whose head is `branch` into one [`PrFact`].
///
/// `prs == None` means the lookup was unavailable. An open PR wins over any
/// merged one, because branch names get reused. Among merged PRs, any whose
/// head covers the tip proves the work landed; otherwise an undeterminable
/// coverage beats a definite "not covered" (both spare, but the reason
/// should not claim more than we know).
pub(crate) fn pr_fact_for_branch(
    branch: &str,
    prs: Option<&[PrRecord]>,
    covered: &mut dyn FnMut(&str) -> Option<bool>,
) -> PrFact {
    let Some(prs) = prs else {
        return PrFact::Unavailable;
    };
    let mine: Vec<&PrRecord> = prs.iter().filter(|p| p.head_ref_name == branch).collect();
    if let Some(open) = mine.iter().find(|p| p.state.eq_ignore_ascii_case("OPEN")) {
        return PrFact::Open {
            number: open.number,
        };
    }
    let mut merged: Vec<&&PrRecord> = mine
        .iter()
        .filter(|p| p.state.eq_ignore_ascii_case("MERGED"))
        .collect();
    if merged.is_empty() {
        return PrFact::NoPr;
    }
    merged.sort_by_key(|p| std::cmp::Reverse(p.number));
    let mut first_unknown = None;
    let mut first_uncovered = None;
    for pr in &merged {
        match covered(&pr.head_ref_oid) {
            Some(true) => {
                return PrFact::Merged {
                    number: pr.number,
                    tip_covered: Some(true),
                }
            }
            None => {
                first_unknown.get_or_insert(pr.number);
            }
            Some(false) => {
                first_uncovered.get_or_insert(pr.number);
            }
        }
    }
    match (first_unknown, first_uncovered) {
        (Some(number), _) => PrFact::Merged {
            number,
            tip_covered: None,
        },
        (None, Some(number)) => PrFact::Merged {
            number,
            tip_covered: Some(false),
        },
        (None, None) => PrFact::NoPr,
    }
}

/// Pure: a `git diff -U0` rendering with the parts that differ between two
/// textually identical changes removed (blob ids and hunk line numbers), so
/// the branch's cumulative change can be compared with a squash commit.
pub(crate) fn normalize_diff(raw: &str) -> String {
    raw.lines()
        .filter(|l| !l.starts_with("index ") && !l.starts_with("@@"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    match probe_cmd("git", cwd, args, GIT_TIMEOUT)? {
        (0, out) => Some(out),
        _ => None,
    }
}

/// Look up this repo's PRs once per tick. `None` unless the origin is a
/// GitHub remote and `gh` answers with parseable JSON in time.
pub(crate) fn lookup_prs(repo_root: &Path) -> Option<Vec<PrRecord>> {
    let url = git(repo_root, &["remote", "get-url", "origin"])?;
    if !url.contains("github.com") {
        return None;
    }
    let (code, out) = probe_cmd(
        "gh",
        repo_root,
        &[
            "pr",
            "list",
            "--state",
            "all",
            "--limit",
            "500",
            "--json",
            "number,state,headRefName,headRefOid",
        ],
        GH_TIMEOUT,
    )?;
    if code != 0 {
        return None;
    }
    serde_json::from_str(&out).ok()
}

fn default_ref(cwd: &Path) -> Option<String> {
    if let Some(name) = git(
        cwd,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        let name = name.trim();
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    ["origin/main", "origin/master"]
        .into_iter()
        .find(|c| git(cwd, &["rev-parse", "--verify", "--quiet", c]).is_some())
        .map(str::to_string)
}

/// `(dirty, untracked)` from one porcelain status call.
fn work_tree_status(cwd: &Path) -> (Option<bool>, Option<bool>) {
    let Some(out) = git(
        cwd,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "--untracked-files=normal",
        ],
    ) else {
        return (None, None);
    };
    let mut dirty = false;
    let mut untracked = false;
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        if line.starts_with("??") {
            untracked = true;
        } else {
            dirty = true;
        }
    }
    (Some(dirty), Some(untracked))
}

/// Whether `tip` is `head` or an ancestor of it. `None` when git cannot say
/// (for example `head` is not present locally).
fn tip_covered_by(cwd: &Path, tip: &str, head: &str) -> Option<bool> {
    if tip == head {
        return Some(true);
    }
    match probe_cmd(
        "git",
        cwd,
        &["merge-base", "--is-ancestor", tip, head],
        GIT_TIMEOUT,
    )? {
        (0, _) => Some(true),
        (1, _) => Some(false),
        _ => None,
    }
}

/// Fallback evidence: the branch's cumulative change since its merge-base
/// appears verbatim as one commit on the default branch (a squash merge).
/// `Some(false)` for a branch with no commits of its own.
fn patch_landed(cwd: &Path, tip: &str) -> Option<bool> {
    let upstream = default_ref(cwd)?;
    let base = git(cwd, &["merge-base", &upstream, tip])?;
    let base = base.trim();
    let ahead = git(cwd, &["rev-list", "--count", &format!("{base}..{tip}")])?;
    if ahead.trim().parse::<u64>().ok()? == 0 {
        return Some(false);
    }
    let branch_diff = normalize_diff(&git(
        cwd,
        &["diff", "-U0", "--no-color", "--no-ext-diff", base, tip],
    )?);
    if branch_diff.trim().is_empty() {
        return Some(false);
    }
    let candidates = git(
        cwd,
        &[
            "rev-list",
            "--no-merges",
            "--max-count",
            PATCH_SCAN_LIMIT,
            &format!("{base}..{upstream}"),
        ],
    )?;
    for commit in candidates.split_whitespace() {
        let Some(diff) = git(
            cwd,
            &[
                "diff",
                "-U0",
                "--no-color",
                "--no-ext-diff",
                &format!("{commit}^"),
                commit,
            ],
        ) else {
            continue;
        };
        if normalize_diff(&diff) == branch_diff {
            return Some(true);
        }
    }
    Some(false)
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn locked_by_live_or_unknown_pid(entry: &WorktreeEntry) -> bool {
    if !entry.locked {
        return false;
    }
    // A lock with no parseable pid is doubt, and doubt spares.
    match entry
        .locked_reason
        .as_deref()
        .and_then(extract_pid_from_lock_reason)
    {
        Some(pid) => OsLivenessProbe.is_alive(pid),
        None => true,
    }
}

fn mtime_unix(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Gather facts for one worktree entry. `prs` is the repo's lookup result.
fn facts_for(
    entry: &WorktreeEntry,
    is_main: bool,
    live_cwds: &[PathBuf],
    prs: Option<&[PrRecord]>,
) -> RepoWorktreeFacts {
    let path = &entry.path;
    let path_exists = path.try_exists().unwrap_or(true);
    let canon = canonical(path);
    let process_inside = live_cwds.iter().any(|cwd| cwd.starts_with(&canon));
    let mut facts = RepoWorktreeFacts {
        evaluated: true,
        path_exists,
        is_main_checkout: is_main,
        locked_live_pid: locked_by_live_or_unknown_pid(entry),
        process_inside,
        detached: entry.detached || entry.branch.is_none(),
        dirty: None,
        untracked: None,
        pr: PrFact::Unavailable,
        patch_landed: None,
    };
    // Every cheaper guard that already spares makes the rest moot.
    if !path_exists
        || facts.is_main_checkout
        || facts.locked_live_pid
        || facts.process_inside
        || facts.detached
    {
        return facts;
    }
    let (dirty, untracked) = work_tree_status(path);
    facts.dirty = dirty;
    facts.untracked = untracked;
    if dirty != Some(false) || untracked != Some(false) {
        return facts;
    }
    let Some(tip) = git(path, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string()) else {
        facts.dirty = None;
        return facts;
    };
    let branch = entry
        .branch
        .as_deref()
        .map(|b| b.strip_prefix("refs/heads/").unwrap_or(b))
        .unwrap_or_default();
    facts.pr = pr_fact_for_branch(branch, prs, &mut |head| tip_covered_by(path, &tip, head));
    if matches!(facts.pr, PrFact::NoPr | PrFact::Unavailable) {
        facts.patch_landed = patch_landed(path, &tip);
    }
    facts
}

/// Probe every worktree of `repo_root`. `prs` is injected so tests can
/// exercise both the merged-PR path and the `gh`-unavailable fallback.
pub(crate) fn probe_repo(
    repo_root: &Path,
    live_cwds: &[PathBuf],
    prs: Option<&[PrRecord]>,
) -> Vec<RepoWorktreeRow> {
    if git_discovery_env_is_poisoned() {
        return Vec::new();
    }
    let Some(raw) = git(repo_root, &["worktree", "list", "--porcelain"]) else {
        return Vec::new();
    };
    let entries = parse_worktree_porcelain(&raw);
    let main_path = entries.first().map(|e| canonical(&e.path));
    let live_cwds: Vec<PathBuf> = live_cwds.iter().map(|p| canonical(p)).collect();
    entries
        .iter()
        .filter(|e| !e.bare)
        .map(|entry| {
            let is_main = main_path.as_deref() == Some(canonical(&entry.path).as_path());
            let facts = facts_for(entry, is_main, &live_cwds, prs);
            RepoWorktreeRow {
                path: entry.path.to_string_lossy().to_string(),
                repo_root: main_path
                    .as_deref()
                    .unwrap_or(repo_root)
                    .to_string_lossy()
                    .to_string(),
                branch: entry
                    .branch
                    .as_deref()
                    .map(|b| b.strip_prefix("refs/heads/").unwrap_or(b).to_string()),
                mtime_unix: mtime_unix(&entry.path),
                verdict: repo_worktree_verdict(&facts),
            }
        })
        .collect()
}

/// Probe every visited repo, one `gh` lookup per repo, deduplicating repos
/// that are worktrees of one another and rows reached through two roots.
pub(crate) fn probe_repos(repo_roots: &[String], live_cwds: &[PathBuf]) -> Vec<RepoWorktreeRow> {
    let mut seen_roots = HashSet::new();
    let mut seen_paths = HashSet::new();
    let mut out = Vec::new();
    for root in repo_roots {
        let root = Path::new(root);
        if !root.try_exists().unwrap_or(false) {
            continue;
        }
        let Some(common) = git(
            root,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        ) else {
            continue;
        };
        if !seen_roots.insert(canonical(Path::new(common.trim()))) {
            continue;
        }
        let prs = lookup_prs(root);
        for row in probe_repo(root, live_cwds, prs.as_deref()) {
            if seen_paths.insert(row.path.clone()) {
                out.push(row);
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "repo_worktree_probe_tests.rs"]
mod tests;
