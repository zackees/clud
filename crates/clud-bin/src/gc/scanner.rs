//! Legacy scanner-root derivation retained after #546 removes client polling.
//!
//! Foreground clients now compute these three roots once and register them with
//! the daemon; they do not own a thread, a per-client `seen` cache, or a poll
//! loop.

use std::path::Path;

#[cfg(test)]
use std::collections::HashSet;
#[cfg(test)]
use std::path::PathBuf;

use crate::daemon::GcWatchRoot;
use crate::worktrees;

#[cfg(test)]
use super::reconcile::{path_matches_repo_root, ScanKind};
#[cfg(test)]
use super::registry::now_unix;
use super::{EXTERN_REPO_KIND, SIBLING_CLONE_KIND, WORKTREE_KIND};
#[cfg(test)]
use super::InsertInput;

/// Injectable effects for the former client scanner's pure discovery seam.
/// The daemon watcher uses the production reconciliation path; these remain
/// so its discovery invariants keep unit coverage without a background thread.
#[cfg(test)]
pub(in crate::gc) struct ScanDeps<'a> {
    pub(in crate::gc) insert: &'a mut dyn FnMut(&InsertInput) -> Result<(), String>,
    pub(in crate::gc) branch_of: &'a dyn Fn(&Path) -> Option<String>,
}

/// Scan one immediate directory level and insert matching paths once. This is
/// deliberately synchronous and side-effect-injected; it is test support, not
/// a client-owned polling loop.
#[cfg(test)]
pub(in crate::gc) fn scan_once_with(
    watch_dir: &Path,
    repo_root: Option<&Path>,
    scan_kind: ScanKind,
    seen: &mut HashSet<PathBuf>,
    deps: &mut ScanDeps<'_>,
) -> Result<(), String> {
    let entries = match std::fs::read_dir(watch_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(format!("read_dir({watch_dir:?}): {err}")),
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !scan_kind.accepts_dir_name(name, repo_root) {
            continue;
        }
        let path = entry.path();
        if matches!(scan_kind, ScanKind::SiblingClone)
            && repo_root.is_some_and(|root| path_matches_repo_root(&path, root))
        {
            continue;
        }
        if seen.contains(&path) {
            continue;
        }
        let input = InsertInput {
            kind: scan_kind.kind().to_string(),
            path: path.to_string_lossy().to_string(),
            repo_root: repo_root.map(|root| root.to_string_lossy().to_string()),
            branch: (deps.branch_of)(&path),
            agent_id: scan_kind.agent_id(name),
            now_unix: now_unix(),
        };
        (deps.insert)(&input)?;
        seen.insert(path);
    }
    Ok(())
}

pub fn watch_roots_for_current_repo() -> Vec<GcWatchRoot> {
    let Ok(root) = worktrees::locate_main_repo_root() else {
        return Vec::new();
    };
    watch_roots_for_repo(&root)
}

pub(crate) fn watch_roots_for_repo(root: &Path) -> Vec<GcWatchRoot> {
    let repo_root = root.to_string_lossy().to_string();
    let mut roots = vec![
        GcWatchRoot {
            kind: WORKTREE_KIND.to_string(),
            watch_dir: root
                .join(".claude")
                .join("worktrees")
                .to_string_lossy()
                .to_string(),
            repo_root: Some(repo_root.clone()),
        },
        GcWatchRoot {
            kind: EXTERN_REPO_KIND.to_string(),
            watch_dir: root.join(".extern-repos").to_string_lossy().to_string(),
            repo_root: Some(repo_root.clone()),
        },
    ];
    if let Some(parent) = root.parent() {
        roots.push(GcWatchRoot {
            kind: SIBLING_CLONE_KIND.to_string(),
            watch_dir: parent.to_string_lossy().to_string(),
            repo_root: Some(repo_root),
        });
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_roots_derivation_matches_legacy_scanner_roots() {
        let root = std::path::Path::new("C:/repo");
        let roots = watch_roots_for_repo(root);
        assert_eq!(roots.len(), 3);
        assert_eq!(roots[0].kind, WORKTREE_KIND);
        assert_eq!(
            std::path::Path::new(&roots[0].watch_dir),
            root.join(".claude/worktrees")
        );
        assert_eq!(roots[1].kind, EXTERN_REPO_KIND);
        assert_eq!(
            std::path::Path::new(&roots[1].watch_dir),
            root.join(".extern-repos")
        );
        assert_eq!(roots[2].kind, SIBLING_CLONE_KIND);
        assert_eq!(roots[2].watch_dir, "C:");
    }
}
