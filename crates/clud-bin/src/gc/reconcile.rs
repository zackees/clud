use std::path::Path;

use super::registry::{now_unix, registry_has_entry};
use super::{GcError, InsertInput, Registry, EXTERN_REPO_KIND, SIBLING_CLONE_KIND, WORKTREE_KIND};
use crate::worktrees;

// ---------- lock-reason pid extraction ----------

/// Extract the pid from a git-worktree lock reason string emitted by
/// Claude Code, e.g. `"claude agent agent-abf (pid 12345)"`. Returns
/// `None` for anything that doesn't match the `pid <digits>` pattern.
pub fn extract_pid_from_lock_reason(reason: &str) -> Option<u32> {
    // Find the `pid ` substring and take the run of ASCII digits that
    // follows. No regex — keeps the dep graph tiny.
    let idx = reason.find("pid ")?;
    let rest = &reason[idx + 4..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

// ---------- reconcile ----------

/// Walk `.claude/worktrees/` in the *current* repo and insert any
/// previously-untracked agent-* subdirectory we find. Returns the number
/// of new entries (i.e. rows that didn't previously exist).
pub fn run_reconcile(registry: &Registry) -> Result<usize, GcError> {
    let main_root = worktrees::locate_main_repo_root().map_err(GcError::Io)?;
    reconcile_repo_root(registry, &main_root)
}

/// Reconcile all tracked directories associated with `main_root`.
///
/// This scans repo-local worktrees, repo-local `.extern-repos/`, and
/// top-level sibling temp clones adjacent to the main checkout.
pub fn reconcile_repo_root(registry: &Registry, main_root: &Path) -> Result<usize, GcError> {
    let watch_dir = main_root.join(".claude").join("worktrees");
    let worktree_res = reconcile_dir(registry, &watch_dir, Some(main_root))?;
    let extern_res =
        reconcile_extern_repos_dir(registry, &main_root.join(".extern-repos"), Some(main_root))?;
    let sibling_res = reconcile_sibling_clones_dir(registry, main_root)?;
    Ok(worktree_res.inserted + extern_res.inserted + sibling_res.inserted)
}

/// Result of one scan pass.
///
/// `skipped` counts rows that were already present in the registry — the
/// scanner's intentional "insert-once" behavior. (Previously the scanner
/// updated `last_seen_unix` on every cycle and reported these as
/// `refreshed`; that field is gone, so the field is renamed to `skipped`
/// to reflect the new contract.)
#[derive(Debug, Default, Clone, Copy)]
pub struct ScanResult {
    pub inserted: usize,
    pub skipped: usize,
}

/// Walk `watch_dir` and insert each immediate subdir whose name starts
/// with `agent-` if it isn't already tracked. Returns counts of
/// inserted-vs-skipped rows.
pub fn reconcile_dir(
    registry: &Registry,
    watch_dir: &Path,
    repo_root: Option<&Path>,
) -> Result<ScanResult, GcError> {
    reconcile_dir_with_kind(registry, watch_dir, repo_root, ScanKind::Worktree)
}

/// Walk `<repo>/.extern-repos/` and insert each immediate child directory
/// as an `extern-repo` row. Nested directories are intentionally ignored.
pub fn reconcile_extern_repos_dir(
    registry: &Registry,
    watch_dir: &Path,
    repo_root: Option<&Path>,
) -> Result<ScanResult, GcError> {
    reconcile_dir_with_kind(registry, watch_dir, repo_root, ScanKind::ExternRepo)
}

/// Walk the parent of `repo_root` and insert immediate child directories
/// that look like disposable sibling clones created by clud/soldr tooling.
///
/// Matching is intentionally narrow: explicit tool prefixes are accepted,
/// and broad `*-wt-*` / `*-issue-*` examples are interpreted only as
/// current-repo-scoped names such as `<repo>-wt-<branch>`.
pub fn reconcile_sibling_clones_dir(
    registry: &Registry,
    repo_root: &Path,
) -> Result<ScanResult, GcError> {
    let Some(parent) = repo_root.parent() else {
        return Ok(ScanResult::default());
    };
    if parent.as_os_str().is_empty() {
        return Ok(ScanResult::default());
    }
    reconcile_dir_with_kind(registry, parent, Some(repo_root), ScanKind::SiblingClone)
}

#[derive(Debug, Clone, Copy)]
pub(in crate::gc) enum ScanKind {
    Worktree,
    ExternRepo,
    SiblingClone,
}

impl ScanKind {
    pub(in crate::gc) fn kind(self) -> &'static str {
        match self {
            Self::Worktree => WORKTREE_KIND,
            Self::ExternRepo => EXTERN_REPO_KIND,
            Self::SiblingClone => SIBLING_CLONE_KIND,
        }
    }

    pub(in crate::gc) fn accepts_dir_name(self, name: &str, repo_root: Option<&Path>) -> bool {
        match self {
            Self::Worktree => name.starts_with("agent-"),
            Self::ExternRepo => true,
            Self::SiblingClone => repo_root
                .and_then(repo_name)
                .map(|repo| is_sibling_clone_dir_name(repo, name))
                .unwrap_or(false),
        }
    }

    pub(in crate::gc) fn agent_id(self, name: &str) -> Option<String> {
        match self {
            Self::Worktree => Some(name.to_string()),
            Self::ExternRepo | Self::SiblingClone => None,
        }
    }
}

fn repo_name(repo_root: &Path) -> Option<&str> {
    repo_root.file_name().and_then(|name| name.to_str())
}

pub(in crate::gc) fn is_sibling_clone_dir_name(repo_name: &str, name: &str) -> bool {
    if repo_name.is_empty() || name == repo_name {
        return false;
    }

    // Explicit temp-clone prefixes used by clud and related tooling.
    // The broader issue examples `*-wt-*` and `*-issue-*` are handled
    // below only when they are scoped to the current repo name.
    const TOOL_PREFIXES: &[&str] = &[
        "clud-pr-",
        "clud-release-",
        "clud-issue-",
        "soldr-wt-",
        "zccache-wt-",
    ];
    if TOOL_PREFIXES
        .iter()
        .any(|prefix| has_nonempty_suffix(name, prefix))
    {
        return true;
    }

    has_nonempty_suffix(name, &format!("{repo_name}-wt-"))
        || has_nonempty_suffix(name, &format!("{repo_name}-issue-"))
}

fn has_nonempty_suffix(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .map(|suffix| !suffix.is_empty())
        .unwrap_or(false)
}

pub(in crate::gc) fn path_matches_repo_root(path: &Path, repo_root: &Path) -> bool {
    if path == repo_root {
        return true;
    }
    match (path.canonicalize(), repo_root.canonicalize()) {
        (Ok(path), Ok(repo_root)) => path == repo_root,
        _ => false,
    }
}

fn reconcile_dir_with_kind(
    registry: &Registry,
    watch_dir: &Path,
    repo_root: Option<&Path>,
    scan_kind: ScanKind,
) -> Result<ScanResult, GcError> {
    let mut res = ScanResult::default();
    let entries = match std::fs::read_dir(watch_dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(res),
        Err(e) => return Err(GcError::Io(format!("read_dir({:?}): {e}", watch_dir))),
    };
    for entry in entries.flatten() {
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if !ft.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        if !scan_kind.accepts_dir_name(name_str, repo_root) {
            continue;
        }
        let path = entry.path();
        if matches!(scan_kind, ScanKind::SiblingClone)
            && repo_root
                .map(|root| path_matches_repo_root(&path, root))
                .unwrap_or(false)
        {
            continue;
        }
        let path_str = path.to_string_lossy().to_string();

        let existed_before = registry_has_entry(registry, scan_kind.kind(), &path_str)?;

        if existed_before {
            // No-op insert; just bump the skipped counter.
            res.skipped += 1;
            continue;
        }

        let branch = best_effort_branch(&path);
        let input = InsertInput {
            kind: scan_kind.kind().to_string(),
            path: path_str,
            repo_root: repo_root.map(|p| p.to_string_lossy().to_string()),
            branch,
            agent_id: scan_kind.agent_id(name_str),
            now_unix: now_unix(),
        };
        registry.insert_if_new(&input)?;
        res.inserted += 1;
    }
    Ok(res)
}

pub(crate) fn best_effort_branch(path: &Path) -> Option<String> {
    worktrees::run_git(path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "HEAD")
}
