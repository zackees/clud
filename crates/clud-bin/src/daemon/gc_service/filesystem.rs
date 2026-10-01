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
    // Audit before acting (#893): the entry path is user data by definition
    // (it is in the registry), so the line names the kind that authorized it.
    crate::gc::delete_audit::record("gc.entry-delete", Path::new(&entry.path), &entry.kind);
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

/// Remove registered trash rows. A `clud trash` quarantine entry goes as
/// soon as it can be deleted; an `safe-rm` entry (it carries
/// [`crate::rm_tool::TRASH_MANIFEST`]) is kept for
/// [`crate::rm_tool::TRASH_KEEP`] so it can be restored (#1340).
pub(super) fn reap_trash_entries(registry: &Registry) -> Result<(usize, usize), String> {
    reap_trash_entries_at(registry, std::time::SystemTime::now())
}

pub(super) fn reap_trash_entries_at(
    registry: &Registry,
    now: std::time::SystemTime,
) -> Result<(usize, usize), String> {
    let entries = registry
        .list(Some("trash"))
        .map_err(|err| err.to_string())?;
    let mut removed = 0usize;
    let mut failed = 0usize;
    for entry in entries {
        if crate::rm_tool::keep_trash_entry(Path::new(&entry.path), now) {
            continue;
        }
        let path = Path::new(&entry.path);
        if std::fs::symlink_metadata(path)
            .is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound)
        {
            // Already gone (size cap, or removed by hand): drop the row
            // instead of failing on it every tick (#1672).
            registry.delete(entry.id).map_err(|err| err.to_string())?;
            removed += 1;
            continue;
        }
        // Audit before acting (#893).
        crate::gc::delete_audit::record("gc.trash-reap", path, "trash");
        match remove_trash_dir(path) {
            Ok(()) => {
                registry.delete(entry.id).map_err(|err| err.to_string())?;
                eprintln!("[gc] trash: reaped {}", entry.path);
                removed += 1;
            }
            Err(err) => {
                // #1672: this used to be silent; one root-owned entry was
                // "reaped" 373 times with no trace of why it stayed.
                eprintln!("[gc] trash: could not remove {}: {err}", entry.path);
                failed += 1;
            }
        }
    }
    Ok((removed, failed))
}

/// `remove_dir_all` after making the tree owner-writable, as safe-rm's own
/// purge does (#1573): sealed build output is read-only. Files owned by
/// another user (root, from a Docker bind mount) still fail, loudly.
fn remove_trash_dir(path: &Path) -> std::io::Result<()> {
    crate::rm_tool::make_writable(path);
    std::fs::remove_dir_all(path)
}

/// Remove expired `safe-rm` trash entries under `trash_root` that
/// never reached the registry (the call found no daemon to register with).
/// Registered ones are removed by [`reap_trash_entries`] first.
pub(super) fn reap_unregistered_rm_trash(trash_root: &Path, now: std::time::SystemTime) -> usize {
    let mut removed = 0usize;
    for dir in crate::rm_tool::expired_trash_entries(trash_root, now) {
        // Audit before acting (#893).
        crate::gc::delete_audit::record("gc.rm-trash-reap", &dir, "rm-trash expired");
        match remove_trash_dir(&dir) {
            Ok(()) => {
                eprintln!("[gc] trash: reaped {}", dir.display());
                removed += 1;
            }
            Err(err) => eprintln!("[gc] trash: could not remove {}: {err}", dir.display()),
        }
    }
    removed
}
