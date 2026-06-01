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
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::gc::{extract_pid_from_lock_reason, reconcile_dir, InsertInput, Registry, TrackedEntry};
use crate::session_registry::{LivenessProbe, OsLivenessProbe};
use crate::worktrees;

use super::types::{GcOp, GcReply, ListRow};

/// How long a connection thread waits for the registry worker before
/// giving up. Generous because purge can rm-rf large trees synchronously.
pub(super) const WORKER_REPLY_TIMEOUT: Duration = Duration::from_secs(30);

const ENV_GC_TICK_SECS: &str = "CLUD_GC_TICK_SECS";
const DEFAULT_GC_TICK_SECS: u64 = 3600;
const PERIODIC_GC_WORKTREE_STALE_AFTER: &str = "48h";

/// One request handed from a connection thread to the registry worker.
pub(super) struct GcRequestMsg {
    pub(super) op: GcOp,
    pub(super) reply_tx: mpsc::SyncSender<GcReply>,
}

type LiveCwdsProvider = Arc<dyn Fn() -> Vec<PathBuf> + Send + Sync + 'static>;

/// Open the registry and spawn the single worker thread. Returns the
/// sender every connection thread uses to dispatch GC ops. Caller keeps
/// the sender alive for the daemon's lifetime; dropping it stops the
/// worker.
#[cfg(test)]
pub(super) fn spawn_registry_worker() -> std::io::Result<mpsc::Sender<GcRequestMsg>> {
    let registry = Registry::open_default().map_err(std::io::Error::other)?;
    spawn_registry_worker_with(registry)
}

pub(super) fn spawn_registry_worker_for_state(
    state_dir: PathBuf,
) -> std::io::Result<mpsc::Sender<GcRequestMsg>> {
    let registry = Registry::open_default().map_err(std::io::Error::other)?;
    spawn_registry_worker_with_live_cwds(
        registry,
        Arc::new(move || super::sessions::list_live_session_cwds(&state_dir)),
    )
}

/// Same as [`spawn_registry_worker`] but accepts a pre-constructed
/// `Registry`. Tests use this to bind a worker to an isolated `redb`
/// file without depending on the process-global `CLUD_DATA_DB` env var.
#[cfg(test)]
pub(super) fn spawn_registry_worker_with(
    registry: Registry,
) -> std::io::Result<mpsc::Sender<GcRequestMsg>> {
    spawn_registry_worker_with_live_cwds(registry, Arc::new(Vec::<PathBuf>::new))
}

fn spawn_registry_worker_with_live_cwds(
    registry: Registry,
    live_cwds_provider: LiveCwdsProvider,
) -> std::io::Result<mpsc::Sender<GcRequestMsg>> {
    let (tx, rx) = mpsc::channel::<GcRequestMsg>();
    let tick_cadence = gc_tick_cadence_from_env();
    thread::Builder::new()
        .name("clud-gc-registry-worker".to_string())
        .spawn(move || run_worker_loop(registry, rx, tick_cadence, live_cwds_provider))?;
    Ok(tx)
}

fn gc_tick_cadence_from_env() -> Option<Duration> {
    let raw = std::env::var(ENV_GC_TICK_SECS).ok();
    gc_tick_cadence_from_raw(raw.as_deref())
}

fn gc_tick_cadence_from_raw(raw: Option<&str>) -> Option<Duration> {
    let secs = raw
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_GC_TICK_SECS);
    if secs == 0 {
        None
    } else {
        Some(Duration::from_secs(secs))
    }
}

fn run_worker_loop(
    registry: Registry,
    rx: mpsc::Receiver<GcRequestMsg>,
    tick_cadence: Option<Duration>,
    live_cwds_provider: LiveCwdsProvider,
) {
    let Some(tick_cadence) = tick_cadence else {
        while let Ok(msg) = rx.recv() {
            handle_worker_msg(&registry, msg, &live_cwds_provider);
        }
        return;
    };

    let mut next_tick = Instant::now() + tick_cadence;
    loop {
        let timeout = next_tick.saturating_duration_since(Instant::now());
        match rx.recv_timeout(timeout) {
            Ok(msg) => {
                handle_worker_msg(&registry, msg, &live_cwds_provider);
                if Instant::now() >= next_tick {
                    run_periodic_purge_tick(&registry, &live_cwds_provider);
                    next_tick = Instant::now() + tick_cadence;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                run_periodic_purge_tick(&registry, &live_cwds_provider);
                next_tick = Instant::now() + tick_cadence;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn handle_worker_msg(
    registry: &Registry,
    msg: GcRequestMsg,
    live_cwds_provider: &LiveCwdsProvider,
) {
    let reply = process_op_with_live_cwds(registry, msg.op, live_cwds_provider());
    // Hung-up callers are fine — the worker keeps serving the rest.
    let _ = msg.reply_tx.send(reply);
}

fn run_periodic_purge_tick(registry: &Registry, live_cwds_provider: &LiveCwdsProvider) {
    let worktree_reply = process_op_with_live_cwds(
        registry,
        GcOp::Purge {
            duration: Some(PERIODIC_GC_WORKTREE_STALE_AFTER.to_string()),
            kind: Some("worktree".to_string()),
            dry_run: false,
        },
        live_cwds_provider(),
    );
    match worktree_reply {
        GcReply::PurgeOk { removed, skipped } => {
            eprintln!("[clud] gc tick: removed {removed}, skipped {skipped}");
        }
        GcReply::Error { message } => {
            eprintln!("[clud] gc tick: error: {message}");
        }
        other => {
            eprintln!("[clud] gc tick: unexpected reply: {other:?}");
        }
    }

    match reap_trash_entries(registry) {
        Ok((removed, failed)) => {
            if removed > 0 || failed > 0 {
                eprintln!("[clud] gc tick: trash removed {removed}, failed {failed}");
            }
        }
        Err(message) => {
            eprintln!("[clud] gc tick: trash error: {message}");
        }
    }
}

fn process_op_with_live_cwds(registry: &Registry, op: GcOp, live_cwds: Vec<PathBuf>) -> GcReply {
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
            let live_cwds = canonicalize_live_cwds(live_cwds);
            let (purgeable, skipped): (Vec<_>, Vec<_>) = candidates
                .into_iter()
                .partition(|c| !entry_is_live(c, &live_locks, &live_cwds));
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
            let live_cwds = canonicalize_live_cwds(live_cwds);
            if entry_is_live(&target, &live_locks, &live_cwds) {
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

fn canonicalize_live_cwds(live_cwds: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = live_cwds
        .into_iter()
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn entry_is_live(
    entry: &TrackedEntry,
    live_locks: &HashSet<String>,
    live_cwds: &[PathBuf],
) -> bool {
    if entry.kind == "trash" {
        return false;
    }
    if entry.kind == "worktree" && live_locks.contains(&entry.path) {
        return true;
    }
    entry_path_contains_live_cwd(entry, live_cwds)
}

fn entry_path_contains_live_cwd(entry: &TrackedEntry, live_cwds: &[PathBuf]) -> bool {
    let Ok(entry_path) = std::fs::canonicalize(&entry.path) else {
        return false;
    };
    live_cwds
        .iter()
        .any(|cwd| cwd == &entry_path || cwd.starts_with(&entry_path))
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
    } else if entry.kind == "trash" {
        std::fs::remove_dir_all(&entry.path).map_err(|e| e.to_string())?;
    } else {
        let p = Path::new(&entry.path);
        if p.exists() {
            std::fs::remove_dir_all(p).map_err(|e| e.to_string())?;
        }
    }
    registry.delete(entry.id).map_err(|e| e.to_string())
}

fn reap_trash_entries(registry: &Registry) -> Result<(usize, usize), String> {
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
        spawn_test_worker_with_tick(db_path, "0")
    }

    fn spawn_test_worker_with_tick(
        db_path: &Path,
        tick_secs: &str,
    ) -> (
        mpsc::Sender<GcRequestMsg>,
        std::sync::MutexGuard<'static, ()>,
    ) {
        let guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prior_db = std::env::var_os(ENV_DATA_DB);
        let prior_tick = std::env::var_os(ENV_GC_TICK_SECS);
        std::env::set_var(ENV_DATA_DB, db_path);
        std::env::set_var(ENV_GC_TICK_SECS, tick_secs);
        let tx = spawn_registry_worker();
        restore_env_var(ENV_GC_TICK_SECS, prior_tick);
        restore_env_var(ENV_DATA_DB, prior_db);
        let tx = tx.unwrap();
        (tx, guard)
    }

    fn spawn_test_worker_with_live_cwds(
        db_path: &Path,
        live_cwds: Vec<PathBuf>,
    ) -> mpsc::Sender<GcRequestMsg> {
        let registry = Registry::open_at(db_path).expect("open registry");
        spawn_registry_worker_with_live_cwds(registry, Arc::new(move || live_cwds.clone()))
            .expect("spawn registry worker")
    }

    fn restore_env_var(key: &str, prior: Option<std::ffi::OsString>) {
        match prior {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    fn call(tx: &mpsc::Sender<GcRequestMsg>, op: GcOp) -> GcReply {
        let (reply_tx, reply_rx) = mpsc::sync_channel::<GcReply>(1);
        tx.send(GcRequestMsg { op, reply_tx }).unwrap();
        reply_rx.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn gc_tick_cadence_config_handles_default_disable_and_positive() {
        assert_eq!(
            gc_tick_cadence_from_raw(None),
            Some(Duration::from_secs(DEFAULT_GC_TICK_SECS))
        );
        assert_eq!(gc_tick_cadence_from_raw(Some("0")), None);
        assert_eq!(
            gc_tick_cadence_from_raw(Some("1")),
            Some(Duration::from_secs(1))
        );
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
    fn purge_skips_entry_equal_to_live_session_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("live-cwd-direct.redb");
        let path_a = dir.path().join("A");
        let path_b = dir.path().join("B");
        std::fs::create_dir_all(&path_a).unwrap();
        std::fs::create_dir_all(&path_b).unwrap();
        let tx = spawn_test_worker_with_live_cwds(&db_path, vec![path_a.clone()]);

        for path in [&path_a, &path_b] {
            call(
                &tx,
                GcOp::Insert {
                    kind: "cache".to_string(),
                    path: path.to_string_lossy().to_string(),
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
                assert_eq!(removed, 1);
                assert_eq!(skipped, 1);
            }
            other => panic!("unexpected reply: {other:?}"),
        }

        assert!(path_a.exists(), "live cwd entry should remain on disk");
        assert!(!path_b.exists(), "non-live entry should be deleted");
        let rows = match call(&tx, GcOp::List { kind: None }) {
            GcReply::ListOk { rows } => rows,
            other => panic!("unexpected reply: {other:?}"),
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, path_a.to_string_lossy().to_string());
    }

    #[test]
    fn purge_skips_entry_that_is_ancestor_of_live_session_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("live-cwd-ancestor.redb");
        let path_a = dir.path().join("A");
        let live_subdir = path_a.join("sub");
        std::fs::create_dir_all(&live_subdir).unwrap();
        let tx = spawn_test_worker_with_live_cwds(&db_path, vec![live_subdir]);

        call(
            &tx,
            GcOp::Insert {
                kind: "cache".to_string(),
                path: path_a.to_string_lossy().to_string(),
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
                dry_run: false,
            },
        );
        match resp {
            GcReply::PurgeOk { removed, skipped } => {
                assert_eq!(removed, 0);
                assert_eq!(skipped, 1);
            }
            other => panic!("unexpected reply: {other:?}"),
        }

        assert!(
            path_a.exists(),
            "ancestor of live cwd should remain on disk"
        );
        let rows = match call(&tx, GcOp::List { kind: None }) {
            GcReply::ListOk { rows } => rows,
            other => panic!("unexpected reply: {other:?}"),
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, path_a.to_string_lossy().to_string());
    }

    #[test]
    fn periodic_tick_removes_old_worktree_entry() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("periodic-purge.redb");
        let (tx, _g) = spawn_test_worker_with_tick(&db_path, "1");
        let old_path = dir.path().join("old-worktree");
        std::fs::create_dir_all(&old_path).unwrap();

        let resp = call(
            &tx,
            GcOp::Insert {
                kind: "worktree".to_string(),
                path: old_path.to_string_lossy().to_string(),
                repo_root: Some(dir.path().to_string_lossy().to_string()),
                branch: Some("stale".to_string()),
                agent_id: Some("agent-old".to_string()),
                created_unix: Some(now_unix().saturating_sub(49 * 60 * 60)),
            },
        );
        assert!(matches!(resp, GcReply::InsertOk));

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let rows = match call(
                &tx,
                GcOp::List {
                    kind: Some("worktree".to_string()),
                },
            ) {
                GcReply::ListOk { rows } => rows,
                other => panic!("unexpected reply: {other:?}"),
            };
            if rows.is_empty() {
                assert!(!old_path.exists());
                break;
            }
            if Instant::now() >= deadline {
                panic!("periodic purge did not remove old worktree entry within 3 seconds");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    fn trash_reaper_deletes_successful_entry_and_row() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("trash-reap.redb");
        let registry = Registry::open_at(&db_path).unwrap();
        let trash_dir = dir.path().join("trash-item");
        std::fs::create_dir_all(&trash_dir).unwrap();
        registry
            .insert_if_new(&InsertInput {
                kind: "trash".to_string(),
                path: trash_dir.to_string_lossy().to_string(),
                repo_root: None,
                branch: None,
                agent_id: Some("C:/repo/target/debug/foo.dll".to_string()),
                now_unix: 100,
            })
            .unwrap();

        let (removed, failed) = reap_trash_entries(&registry).unwrap();

        assert_eq!((removed, failed), (1, 0));
        assert!(!trash_dir.exists());
        assert!(registry.list(Some("trash")).unwrap().is_empty());
    }

    #[test]
    fn trash_reaper_keeps_row_when_delete_fails() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("trash-reap-fail.redb");
        let registry = Registry::open_at(&db_path).unwrap();
        let not_a_dir = dir.path().join("still-locked.dll");
        std::fs::write(&not_a_dir, b"locked").unwrap();
        registry
            .insert_if_new(&InsertInput {
                kind: "trash".to_string(),
                path: not_a_dir.to_string_lossy().to_string(),
                repo_root: None,
                branch: None,
                agent_id: Some("C:/repo/target/debug/still-locked.dll".to_string()),
                now_unix: 100,
            })
            .unwrap();

        let (removed, failed) = reap_trash_entries(&registry).unwrap();

        assert_eq!((removed, failed), (0, 1));
        assert!(not_a_dir.exists());
        assert_eq!(registry.list(Some("trash")).unwrap().len(), 1);
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
