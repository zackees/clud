use std::collections::HashSet;
use std::path::Path;

use crate::gc::{extract_pid_from_lock_reason, Registry, TrackedEntry};
use crate::session_registry::{LivenessProbe, OsLivenessProbe};
use crate::worktrees;

/// Paths that `git worktree list --porcelain` reports as `locked` with a
/// reason of the form `agent <pid>` where the PID is still alive. Used to
/// shield in-flight `clud` worktrees from `clud gc purge`.
pub(super) fn collect_live_lock_paths() -> HashSet<String> {
    let mut out = HashSet::new();
    let probe = OsLivenessProbe;
    let main_root = match worktrees::locate_main_repo_root() {
        Ok(p) => p,
        Err(_) => return out,
    };
    let raw = match worktrees::run_git(&main_root, &["worktree", "list", "--porcelain"]) {
        Ok(s) => s,
        Err(_) => return out,
    };
    let entries = worktrees::parse_worktree_porcelain(&raw);
    for e in entries {
        if !e.locked {
            continue;
        }
        let Some(reason) = e.locked_reason.as_deref() else {
            continue;
        };
        let Some(pid) = extract_pid_from_lock_reason(reason) else {
            continue;
        };
        if probe.is_alive(pid) {
            out.insert(e.path.to_string_lossy().to_string());
        }
    }
    out
}

/// Filesystem-only half of removing one tracked entry. Used by both
/// the synchronous path (`DeleteById`) and the parallel purge pool
/// (`PurgeJob`). Safe to call from any thread — does not touch redb.
pub(super) fn remove_entry_filesystem(entry: &TrackedEntry) -> Result<(), String> {
    if entry.kind == "worktree" {
        let main_root = entry.repo_root.clone().unwrap_or_else(|| ".".to_string());
        let _ =
            worktrees::remove_worktree_path(Path::new(&main_root), Path::new(&entry.path), true)?;
        Ok(())
    } else if entry.kind == "trash" {
        std::fs::remove_dir_all(&entry.path).map_err(|e| e.to_string())
    } else {
        let p = Path::new(&entry.path);
        if p.exists() {
            std::fs::remove_dir_all(p).map_err(|e| e.to_string())
        } else {
            Ok(())
        }
    }
}

/// Synchronous "remove filesystem entry, then drop redb row" — used by
/// the per-row `GcOp::DeleteById` path which still needs the dashboard
/// to see the row gone before the response returns. Bulk purges use
/// the async fan-out path via `dispatch_purge_entries` instead.
pub(super) fn remove_entry_and_delete_row(
    registry: &Registry,
    entry: &TrackedEntry,
) -> Result<(), String> {
    remove_entry_filesystem(entry)?;
    registry.delete(entry.id).map_err(|e| e.to_string())
}

pub(super) fn reap_trash_entries(registry: &Registry) -> Result<(usize, usize), String> {
    let entries = registry
        .list(Some("trash"))
        .map_err(|err| err.to_string())?;
    let mut removed = 0usize;
    let mut failed = 0usize;
    for entry in entries {
        match std::fs::remove_dir_all(&entry.path) {
            Ok(()) => {
                registry.delete(entry.id).map_err(|err| err.to_string())?;
                eprintln!("[gc] trash: reaped {}", entry.path);
                removed += 1;
            }
            Err(_) => {
                failed += 1;
            }
        }
    }
    Ok((removed, failed))
}
