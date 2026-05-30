//! GC service running inside the always-on session daemon.
//!
//! Owns the redb registry exclusively (issue #135). All `clud gc *`
//! IPC ops on the session daemon's TCP listener get routed to a single
//! registry worker thread; the worker is the sole reader/writer of
//! `~/.clud/data.redb`. This module replaces the standalone `gc_daemon`
//! process that shipped in Phase 1 of #135 — there is now exactly one
//! background daemon per user (see [docs/architecture/gc-and-registry.md]).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::gc::{extract_pid_from_lock_reason, reconcile_dir, InsertInput, Registry, TrackedEntry};
use crate::session_registry::{LivenessProbe, OsLivenessProbe};
use crate::worktrees;

use super::types::{GcOp, GcReply, ListRow};

/// How long a connection thread waits for the registry worker before
/// giving up. Generous because purge can rm-rf large trees synchronously.
pub(super) const WORKER_REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// One request handed from a connection thread to the registry worker.
pub(super) struct GcRequestMsg {
    pub(super) op: GcOp,
    pub(super) reply_tx: mpsc::SyncSender<GcReply>,
}

/// Open the registry and spawn the single worker thread. Returns the
/// sender every connection thread uses to dispatch GC ops. Caller keeps
/// the sender alive for the daemon's lifetime; dropping it stops the
/// worker.
pub(super) fn spawn_registry_worker() -> std::io::Result<mpsc::Sender<GcRequestMsg>> {
    let registry = Registry::open_default().map_err(std::io::Error::other)?;
    spawn_registry_worker_with(registry)
}

/// Same as [`spawn_registry_worker`] but accepts a pre-constructed
/// `Registry`. Tests use this to bind a worker to an isolated `redb`
/// file without depending on the process-global `CLUD_DATA_DB` env var.
pub(super) fn spawn_registry_worker_with(
    registry: Registry,
) -> std::io::Result<mpsc::Sender<GcRequestMsg>> {
    let (tx, rx) = mpsc::channel::<GcRequestMsg>();
    thread::Builder::new()
        .name("clud-gc-registry-worker".to_string())
        .spawn(move || run_worker_loop(registry, rx))?;
    Ok(tx)
}

fn run_worker_loop(registry: Registry, rx: mpsc::Receiver<GcRequestMsg>) {
    while let Ok(msg) = rx.recv() {
        let reply = process_op(&registry, msg.op);
        // Hung-up callers are fine — the worker keeps serving the rest.
        let _ = msg.reply_tx.send(reply);
    }
}

pub(super) fn process_op(registry: &Registry, op: GcOp) -> GcReply {
    match op {
        GcOp::List { kind } => match registry.list(kind.as_deref()) {
            Ok(rows) => {
                let live_locks = collect_live_lock_paths();
                let out: Vec<ListRow> = rows
                    .into_iter()
                    .map(|r| ListRow {
                        live_locked: r.kind == "worktree" && live_locks.contains(&r.path),
                        id: r.id,
                        kind: r.kind,
                        path: r.path,
                        repo_root: r.repo_root,
                        branch: r.branch,
                        agent_id: r.agent_id,
                        created_unix: r.created_unix,
                    })
                    .collect();
                GcReply::ListOk { rows: out }
            }
            Err(e) => GcReply::Error {
                message: e.to_string(),
            },
        },

        GcOp::Purge {
            duration,
            kind,
            dry_run,
        } => {
            let candidates_res = match &duration {
                Some(d) => match worktrees::parse_duration(d) {
                    Ok(dur) => {
                        let cutoff = now_unix().saturating_sub(dur.as_secs() as i64);
                        registry.select_older_than(cutoff, kind.as_deref())
                    }
                    Err(e) => {
                        return GcReply::Error {
                            message: format!("invalid duration: {e}"),
                        };
                    }
                },
                None => registry.list(kind.as_deref()),
            };
            let candidates: Vec<TrackedEntry> = match candidates_res {
                Ok(v) => v,
                Err(e) => {
                    return GcReply::Error {
                        message: e.to_string(),
                    };
                }
            };
            let live_locks = collect_live_lock_paths();
            let (purgeable, skipped): (Vec<_>, Vec<_>) = candidates
                .into_iter()
                .partition(|c| !(c.kind == "worktree" && live_locks.contains(&c.path)));
            if dry_run {
                return GcReply::PurgeOk {
                    removed: purgeable.len(),
                    skipped: skipped.len(),
                };
            }
            let mut removed = 0usize;
            for entry in &purgeable {
                if remove_entry_and_delete_row(registry, entry).is_ok() {
                    removed += 1;
                }
            }
            GcReply::PurgeOk {
                removed,
                skipped: skipped.len(),
            }
        }

        GcOp::Reconcile { repo_root } => {
            let root = PathBuf::from(&repo_root);
            let watch_dir = root.join(".claude").join("worktrees");
            match reconcile_dir(registry, &watch_dir, Some(&root)) {
                Ok(res) => GcReply::ReconcileOk {
                    inserted: res.inserted,
                },
                Err(e) => GcReply::Error {
                    message: e.to_string(),
                },
            }
        }

        GcOp::Insert {
            kind,
            path,
            repo_root,
            branch,
            agent_id,
            created_unix,
        } => {
            let input = InsertInput {
                kind,
                path,
                repo_root,
                branch,
                agent_id,
                now_unix: created_unix.unwrap_or_else(now_unix),
            };
            match registry.insert_if_new(&input) {
                Ok(()) => GcReply::InsertOk,
                Err(e) => GcReply::Error {
                    message: e.to_string(),
                },
            }
        }

        GcOp::RecordRepoVisit {
            repo_root,
            cwd,
            now_unix: provided,
        } => {
            let stamp = provided.unwrap_or_else(now_unix);
            match registry.record_repo_visit(&repo_root, &cwd, stamp) {
                Ok(()) => GcReply::RepoVisitOk,
                Err(e) => GcReply::Error {
                    message: e.to_string(),
                },
            }
        }

        GcOp::ListRepoVisits => match registry.list_repo_visits() {
            Ok(rows) => GcReply::RepoVisitsOk { rows },
            Err(e) => GcReply::Error {
                message: e.to_string(),
            },
        },

        GcOp::DeleteById { id } => {
            let entries = match registry.list(None) {
                Ok(v) => v,
                Err(e) => {
                    return GcReply::Error {
                        message: e.to_string(),
                    };
                }
            };
            let Some(target) = entries.into_iter().find(|e| e.id == id) else {
                // Idempotent: an id that no longer exists is `removed=0,
                // skipped=0`. The dashboard refreshes after every delete
                // so a stale id click is silently a no-op.
                return GcReply::PurgeOk {
                    removed: 0,
                    skipped: 0,
                };
            };
            let live_locks = collect_live_lock_paths();
            if target.kind == "worktree" && live_locks.contains(&target.path) {
                return GcReply::PurgeOk {
                    removed: 0,
                    skipped: 1,
                };
            }
            match remove_entry_and_delete_row(registry, &target) {
                Ok(()) => GcReply::PurgeOk {
                    removed: 1,
                    skipped: 0,
                },
                Err(message) => GcReply::Error { message },
            }
        }
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Paths that `git worktree list --porcelain` reports as `locked` with a
/// reason of the form `agent <pid>` where the PID is still alive. Used to
/// shield in-flight `clud` worktrees from `clud gc purge`.
fn collect_live_lock_paths() -> HashSet<String> {
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

fn remove_entry_and_delete_row(registry: &Registry, entry: &TrackedEntry) -> Result<(), String> {
    if entry.kind == "worktree" {
        let main_root = entry.repo_root.clone().unwrap_or_else(|| ".".to_string());
        let git_result = worktrees::run_git(
            Path::new(&main_root),
            &["worktree", "remove", "--force", &entry.path],
        );
        if git_result.is_err() {
            let dir = Path::new(&entry.path);
            if dir.exists() {
                std::fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
            }
        }
    } else {
        let p = Path::new(&entry.path);
        if p.exists() {
            std::fs::remove_dir_all(p).map_err(|e| e.to_string())?;
        }
    }
    registry.delete(entry.id).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::ENV_DATA_DB;
    use std::sync::Mutex;

    // ENV_DATA_DB is process-global; serialize so two test threads
    // never race to open the same redb file concurrently.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Spin up a registry worker against an isolated redb file and return
    /// its sender plus a guard that holds `TEST_LOCK` for the test's
    /// lifetime. The worker thread stops when the returned sender is
    /// dropped.
    fn spawn_test_worker(
        db_path: &Path,
    ) -> (
        mpsc::Sender<GcRequestMsg>,
        std::sync::MutexGuard<'static, ()>,
    ) {
        let guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(ENV_DATA_DB, db_path);
        let tx = spawn_registry_worker().unwrap();
        (tx, guard)
    }

    fn call(tx: &mpsc::Sender<GcRequestMsg>, op: GcOp) -> GcReply {
        let (reply_tx, reply_rx) = mpsc::sync_channel::<GcReply>(1);
        tx.send(GcRequestMsg { op, reply_tx }).unwrap();
        reply_rx.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn round_trip_insert_then_list() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.redb");
        let (tx, _g) = spawn_test_worker(&db_path);

        let resp = call(
            &tx,
            GcOp::Insert {
                kind: "worktree".to_string(),
                path: "/tmp/test-a".to_string(),
                repo_root: Some("/tmp/repo".to_string()),
                branch: Some("main".to_string()),
                agent_id: Some("agent-abc".to_string()),
                created_unix: Some(100),
            },
        );
        assert!(matches!(resp, GcReply::InsertOk));

        let resp = call(&tx, GcOp::List { kind: None });
        match resp {
            GcReply::ListOk { rows } => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].path, "/tmp/test-a");
                assert_eq!(rows[0].agent_id.as_deref(), Some("agent-abc"));
            }
            other => panic!("unexpected reply: {other:?}"),
        }
    }

    #[test]
    fn purge_with_no_duration_removes_all_non_live() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("purge-all.redb");
        let (tx, _g) = spawn_test_worker(&db_path);

        for path in ["/tmp/c1", "/tmp/c2"] {
            call(
                &tx,
                GcOp::Insert {
                    kind: "cache".to_string(),
                    path: path.to_string(),
                    repo_root: None,
                    branch: None,
                    agent_id: None,
                    created_unix: Some(100),
                },
            );
        }

        let resp = call(
            &tx,
            GcOp::Purge {
                duration: None,
                kind: None,
                dry_run: false,
            },
        );
        match resp {
            GcReply::PurgeOk { removed, skipped } => {
                assert_eq!(removed, 2);
                assert_eq!(skipped, 0);
            }
            other => panic!("unexpected reply: {other:?}"),
        }

        let resp = call(&tx, GcOp::List { kind: None });
        match resp {
            GcReply::ListOk { rows } => assert!(rows.is_empty()),
            other => panic!("unexpected reply: {other:?}"),
        }
    }

    #[test]
    fn purge_dry_run_does_not_modify_db() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("purge-dry.redb");
        let (tx, _g) = spawn_test_worker(&db_path);

        call(
            &tx,
            GcOp::Insert {
                kind: "cache".to_string(),
                path: "/tmp/dry".to_string(),
                repo_root: None,
                branch: None,
                agent_id: None,
                created_unix: Some(100),
            },
        );
        let resp = call(
            &tx,
            GcOp::Purge {
                duration: None,
                kind: None,
                dry_run: true,
            },
        );
        match resp {
            GcReply::PurgeOk { removed, .. } => assert_eq!(removed, 1),
            other => panic!("unexpected reply: {other:?}"),
        }
        let resp = call(&tx, GcOp::List { kind: None });
        match resp {
            GcReply::ListOk { rows } => assert_eq!(rows.len(), 1),
            other => panic!("unexpected reply: {other:?}"),
        }
    }

    #[test]
    fn list_filter_by_kind() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("filter.redb");
        let (tx, _g) = spawn_test_worker(&db_path);

        call(
            &tx,
            GcOp::Insert {
                kind: "worktree".to_string(),
                path: "/tmp/wt".to_string(),
                repo_root: None,
                branch: None,
                agent_id: None,
                created_unix: Some(100),
            },
        );
        call(
            &tx,
            GcOp::Insert {
                kind: "cache".to_string(),
                path: "/tmp/ca".to_string(),
                repo_root: None,
                branch: None,
                agent_id: None,
                created_unix: Some(100),
            },
        );
        let resp = call(
            &tx,
            GcOp::List {
                kind: Some("worktree".to_string()),
            },
        );
        match resp {
            GcReply::ListOk { rows } => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].kind, "worktree");
            }
            other => panic!("unexpected reply: {other:?}"),
        }
    }

    /// Issue #183: per-row Delete must target exactly the requested id
    /// regardless of how many siblings share its kind. Earlier iterations
    /// of the dashboard worked around the missing IPC primitive by
    /// issuing `Purge { kind: Some(k) }` and refusing when k had >1 row,
    /// which broke the per-row button in the common multi-row case.
    #[test]
    fn delete_by_id_removes_only_the_targeted_row() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("delete-by-id.redb");
        let (tx, _g) = spawn_test_worker(&db_path);

        // Three rows of the same kind — the bug case the workaround
        // refused to handle.
        let paths = [
            dir.path().join("e1").to_string_lossy().to_string(),
            dir.path().join("e2").to_string_lossy().to_string(),
            dir.path().join("e3").to_string_lossy().to_string(),
        ];
        for p in &paths {
            std::fs::create_dir_all(p).unwrap();
            call(
                &tx,
                GcOp::Insert {
                    kind: "cache".to_string(),
                    path: p.clone(),
                    repo_root: None,
                    branch: None,
                    agent_id: None,
                    created_unix: Some(100),
                },
            );
        }

        // Snapshot the rows so we can pick the middle id by stable mapping.
        let list = match call(&tx, GcOp::List { kind: None }) {
            GcReply::ListOk { rows } => rows,
            other => panic!("unexpected reply: {other:?}"),
        };
        assert_eq!(list.len(), 3);
        let middle = list
            .iter()
            .find(|r| r.path == paths[1])
            .expect("middle row");

        let resp = call(&tx, GcOp::DeleteById { id: middle.id });
        match resp {
            GcReply::PurgeOk { removed, skipped } => {
                assert_eq!(removed, 1);
                assert_eq!(skipped, 0);
            }
            other => panic!("unexpected reply: {other:?}"),
        }

        // The two siblings must survive.
        let after = match call(&tx, GcOp::List { kind: None }) {
            GcReply::ListOk { rows } => rows,
            other => panic!("unexpected reply: {other:?}"),
        };
        let remaining: Vec<&str> = after.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(after.len(), 2);
        assert!(remaining.contains(&paths[0].as_str()));
        assert!(remaining.contains(&paths[2].as_str()));
        assert!(!remaining.contains(&paths[1].as_str()));

        // The on-disk path for the targeted row should be gone too.
        assert!(!std::path::Path::new(&paths[1]).exists());
        // Siblings should still be on disk.
        assert!(std::path::Path::new(&paths[0]).exists());
        assert!(std::path::Path::new(&paths[2]).exists());
    }

    /// Deleting a non-existent id is idempotent (`removed=0, skipped=0`).
    /// Lets the dashboard refresh-then-click race resolve without a 500.
    #[test]
    fn delete_by_id_with_missing_id_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("delete-missing.redb");
        let (tx, _g) = spawn_test_worker(&db_path);

        let resp = call(&tx, GcOp::DeleteById { id: 9_999_999 });
        match resp {
            GcReply::PurgeOk { removed, skipped } => {
                assert_eq!(removed, 0);
                assert_eq!(skipped, 0);
            }
            other => panic!("unexpected reply: {other:?}"),
        }
    }
}
