use super::*;
use crate::gc::ENV_DATA_DB;
use std::ffi::OsString;
use std::fs;
use std::sync::Mutex;

#[path = "tests/parallel.rs"]
mod parallel;

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
    mpsc::Sender<RegistryMsg>,
    std::sync::MutexGuard<'static, ()>,
) {
    spawn_test_worker_with_tick(db_path, "0")
}

fn spawn_test_worker_with_tick(
    db_path: &Path,
    tick_secs: &str,
) -> (
    mpsc::Sender<RegistryMsg>,
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
) -> mpsc::Sender<RegistryMsg> {
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

struct ScopedEnv {
    key: &'static str,
    prior: Option<OsString>,
}

impl ScopedEnv {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let prior = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, prior }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        restore_env_var(self.key, self.prior.take());
    }
}

fn call(tx: &mpsc::Sender<RegistryMsg>, op: GcOp) -> GcReply {
    let (reply_tx, reply_rx) = mpsc::sync_channel::<GcReply>(1);
    tx.send(RegistryMsg::Op(GcRequestMsg { op, reply_tx }))
        .unwrap();
    reply_rx.recv_timeout(Duration::from_secs(5)).unwrap()
}

/// Block (polling `GcOp::List`) until the worker reports
/// `target_count` rows of the given kind, or `timeout` elapses.
/// Used to bridge the asynchronous gap between a bulk
/// `PurgeStarted` reply and the matching completions reaching the
/// worker thread.
fn wait_for_row_count(
    tx: &mpsc::Sender<RegistryMsg>,
    kind: Option<&str>,
    target_count: usize,
    timeout: Duration,
) -> Vec<ListRow> {
    let deadline = Instant::now() + timeout;
    loop {
        let rows = match call(
            tx,
            GcOp::List {
                kind: kind.map(String::from),
            },
        ) {
            GcReply::ListOk { rows } => rows,
            other => panic!("unexpected reply: {other:?}"),
        };
        if rows.len() == target_count || Instant::now() >= deadline {
            return rows;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Drain `RegistryMsg::PurgeCompletion(..)` items from `rx` until
/// either no completion arrives within `quiet_for` or `timeout`
/// elapses, applying each one against `registry`. Used by tests
/// that drive the periodic-tick helpers directly — outside the
/// worker loop the test plays the role of the worker.
fn drain_purge_completions(
    registry: &Registry,
    rx: &mpsc::Receiver<RegistryMsg>,
    quiet_for: Duration,
    timeout: Duration,
) -> usize {
    let deadline = Instant::now() + timeout;
    let mut drained = 0usize;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return drained;
        }
        let wait = quiet_for.min(remaining);
        match rx.recv_timeout(wait) {
            Ok(RegistryMsg::PurgeCompletion(c)) => {
                apply_purge_completion(registry, c);
                drained += 1;
            }
            Ok(RegistryMsg::Op(_)) => {
                // Tests don't drive ops through this channel; ignore.
            }
            Err(_) => return drained,
        }
    }
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
fn gc_disk_watchdog_config_parses_defaults_and_overrides() {
    let defaults = gc_disk_watchdog_config_from_raw(None, None, None, None);
    assert_eq!(defaults.warn_free_bytes, 10 * BYTES_PER_GB);
    assert_eq!(defaults.auto_purge_free_bytes, 5 * BYTES_PER_GB);
    assert_eq!(defaults.min_age, Duration::from_secs(24 * 60 * 60));
    assert!(defaults.auto_purge_enabled);

    let overrides =
        gc_disk_watchdog_config_from_raw(Some("1.5"), Some("2"), Some("7"), Some("off"));
    assert_eq!(overrides.warn_free_bytes, BYTES_PER_GB + BYTES_PER_GB / 2);
    assert_eq!(overrides.auto_purge_free_bytes, 2 * BYTES_PER_GB);
    assert_eq!(overrides.min_age, Duration::from_secs(7 * 60 * 60));
    assert!(!overrides.auto_purge_enabled);
}

#[test]
fn gc_disk_watchdog_config_falls_back_on_invalid_values() {
    let config =
        gc_disk_watchdog_config_from_raw(Some("-1"), Some("nan"), Some("bad"), Some("maybe"));
    assert_eq!(config.warn_free_bytes, 10 * BYTES_PER_GB);
    assert_eq!(config.auto_purge_free_bytes, 5 * BYTES_PER_GB);
    assert_eq!(config.min_age, Duration::from_secs(24 * 60 * 60));
    assert!(config.auto_purge_enabled);
}

#[test]
fn disk_watchdog_decision_warns_and_purges_only_below_thresholds() {
    let config = GcDiskWatchdogConfig {
        warn_free_bytes: 10 * BYTES_PER_GB,
        auto_purge_free_bytes: 5 * BYTES_PER_GB,
        min_age: Duration::from_secs(24 * 60 * 60),
        auto_purge_enabled: true,
    };

    assert_eq!(
        disk_watchdog_decision(&config, 10 * BYTES_PER_GB),
        DiskWatchdogDecision {
            warn: false,
            auto_purge: false
        }
    );
    assert_eq!(
        disk_watchdog_decision(&config, 9 * BYTES_PER_GB),
        DiskWatchdogDecision {
            warn: true,
            auto_purge: false
        }
    );
    assert_eq!(
        disk_watchdog_decision(&config, 4 * BYTES_PER_GB),
        DiskWatchdogDecision {
            warn: true,
            auto_purge: true
        }
    );

    let disabled = GcDiskWatchdogConfig {
        auto_purge_enabled: false,
        ..config
    };
    assert_eq!(
        disk_watchdog_decision(&disabled, 4 * BYTES_PER_GB),
        DiskWatchdogDecision {
            warn: true,
            auto_purge: false
        }
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
        GcReply::PurgeStarted {
            dispatched,
            skipped,
        } => {
            assert_eq!(dispatched, 2);
            assert_eq!(skipped, 0);
        }
        other => panic!("unexpected reply: {other:?}"),
    }

    // Issue #268: bulk purge dispatches to the pool and returns
    // immediately. Wait for the pool to finish + the worker to
    // apply the completions before asserting the registry is
    // empty.
    let rows = wait_for_row_count(&tx, None, 0, Duration::from_secs(5));
    assert!(rows.is_empty(), "expected registry to drain, got {rows:?}");
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
        GcReply::PurgeStarted {
            dispatched,
            skipped,
        } => {
            assert_eq!(dispatched, 1);
            assert_eq!(skipped, 1);
        }
        other => panic!("unexpected reply: {other:?}"),
    }

    // Wait for the async delete of path_b to land in redb.
    let rows = wait_for_row_count(&tx, None, 1, Duration::from_secs(5));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path, path_a.to_string_lossy().to_string());
    assert!(path_a.exists(), "live cwd entry should remain on disk");
    assert!(!path_b.exists(), "non-live entry should be deleted");
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
        GcReply::PurgeStarted {
            dispatched,
            skipped,
        } => {
            assert_eq!(dispatched, 0);
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
fn periodic_tick_auto_purges_old_worktree_entry_when_free_space_low() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("periodic-purge.redb");
    let registry = Registry::open_at(&db_path).unwrap();
    let old_path = dir.path().join("old-worktree");
    let old_sibling = dir.path().join("clud-pr-old");
    std::fs::create_dir_all(&old_path).unwrap();
    std::fs::create_dir_all(&old_sibling).unwrap();

    registry
        .insert_if_new(&InsertInput {
            kind: "worktree".to_string(),
            path: old_path.to_string_lossy().to_string(),
            repo_root: Some(dir.path().to_string_lossy().to_string()),
            branch: Some("stale".to_string()),
            agent_id: Some("agent-old".to_string()),
            now_unix: now_unix().saturating_sub(25 * 60 * 60),
        })
        .unwrap();
    registry
        .insert_if_new(&InsertInput {
            kind: SIBLING_CLONE_KIND.to_string(),
            path: old_sibling.to_string_lossy().to_string(),
            repo_root: Some(dir.path().to_string_lossy().to_string()),
            branch: Some("old".to_string()),
            agent_id: None,
            now_unix: now_unix().saturating_sub(25 * 60 * 60),
        })
        .unwrap();

    let config = GcDiskWatchdogConfig {
        warn_free_bytes: 10 * BYTES_PER_GB,
        auto_purge_free_bytes: 5 * BYTES_PER_GB,
        min_age: Duration::from_secs(24 * 60 * 60),
        auto_purge_enabled: true,
    };
    let live_cwds_provider: LiveCwdsProvider = Arc::new(Vec::<PathBuf>::new);
    let pool_tx = spawn_purge_pool(2);
    let (completion_tx, completion_rx) = mpsc::channel::<RegistryMsg>();
    run_periodic_purge_tick_with_free_space(
        &registry,
        &pool_tx,
        &completion_tx,
        &live_cwds_provider,
        &config,
        &|_| Ok(4 * BYTES_PER_GB),
    );
    // Outside the worker loop the test plays the role of the
    // registry-writer thread: drain the pool's completion
    // callbacks and apply them to redb directly.
    // #383: the 250ms quiet window was too tight on Windows, where two
    // parallel directory-purges can finish more than 250ms apart due to
    // AV-scanner / TempDir contention. Use 1500ms quiet so the second
    // completion has room to land; the 5s overall deadline still caps
    // the worst case.
    let drained = drain_purge_completions(
        &registry,
        &completion_rx,
        Duration::from_millis(1500),
        Duration::from_secs(5),
    );
    assert!(
        drained >= 2,
        "expected at least 2 completions, got {drained}"
    );

    assert!(registry.list(Some(WORKTREE_KIND)).unwrap().is_empty());
    assert!(registry.list(Some(SIBLING_CLONE_KIND)).unwrap().is_empty());
    assert!(!old_path.exists());
    assert!(!old_sibling.exists());
}

#[test]
fn periodic_tick_keeps_old_worktree_entry_when_free_space_is_healthy() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("periodic-healthy.redb");
    let registry = Registry::open_at(&db_path).unwrap();
    let old_path = dir.path().join("old-worktree");
    std::fs::create_dir_all(&old_path).unwrap();

    registry
        .insert_if_new(&InsertInput {
            kind: "worktree".to_string(),
            path: old_path.to_string_lossy().to_string(),
            repo_root: Some(dir.path().to_string_lossy().to_string()),
            branch: Some("stale".to_string()),
            agent_id: Some("agent-old".to_string()),
            now_unix: now_unix().saturating_sub(25 * 60 * 60),
        })
        .unwrap();

    let config = GcDiskWatchdogConfig {
        warn_free_bytes: 10 * BYTES_PER_GB,
        auto_purge_free_bytes: 5 * BYTES_PER_GB,
        min_age: Duration::from_secs(24 * 60 * 60),
        auto_purge_enabled: true,
    };
    let live_cwds_provider: LiveCwdsProvider = Arc::new(Vec::<PathBuf>::new);
    let pool_tx = spawn_purge_pool(1);
    let (completion_tx, completion_rx) = mpsc::channel::<RegistryMsg>();
    run_periodic_purge_tick_with_free_space(
        &registry,
        &pool_tx,
        &completion_tx,
        &live_cwds_provider,
        &config,
        &|_| Ok(20 * BYTES_PER_GB),
    );
    // Healthy disk → no dispatches expected, so no completions
    // should land.
    let drained = drain_purge_completions(
        &registry,
        &completion_rx,
        Duration::from_millis(150),
        Duration::from_millis(500),
    );
    assert_eq!(drained, 0);

    assert_eq!(registry.list(Some(WORKTREE_KIND)).unwrap().len(), 1);
    assert!(old_path.exists());
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
fn periodic_tick_removes_stale_extern_repo_entry() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("extern-purge.redb");
    let repo = dir.path().join("extern");
    fs::create_dir_all(&repo).unwrap();

    let _age = ScopedEnv::set(ENV_GC_EXTERN_REPO_MAX_AGE_SECS, "0");

    let registry = Registry::open_at(&db_path).expect("open registry");
    registry
        .insert_if_new(&InsertInput {
            kind: EXTERN_REPO_KIND.to_string(),
            path: repo.to_string_lossy().to_string(),
            repo_root: Some(dir.path().to_string_lossy().to_string()),
            branch: None,
            agent_id: None,
            now_unix: now_unix(),
        })
        .expect("insert extern repo");

    let live_cwds_provider: LiveCwdsProvider = Arc::new(Vec::<PathBuf>::new);
    let pool_tx = spawn_purge_pool(1);
    let (completion_tx, completion_rx) = mpsc::channel::<RegistryMsg>();
    run_periodic_purge_tick(&registry, &pool_tx, &completion_tx, &live_cwds_provider);
    // #383: matches the bump in the periodic-purge test above —
    // 250ms was too tight on Windows for sequential purge completions.
    let _drained = drain_purge_completions(
        &registry,
        &completion_rx,
        Duration::from_millis(1500),
        Duration::from_secs(5),
    );

    let rows = registry.list(Some(EXTERN_REPO_KIND)).expect("list");
    assert!(rows.is_empty(), "stale extern-repo row should be deleted");
    assert!(!repo.exists(), "stale extern-repo dir should be deleted");
}

#[test]
fn periodic_tick_keeps_fresh_extern_repo_entry() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("extern-keep.redb");
    let repo = dir.path().join("extern");
    fs::create_dir_all(&repo).unwrap();

    // 1h stale-after, but the dir was just created (mtime ~ now) → keep.
    let _age = ScopedEnv::set(ENV_GC_EXTERN_REPO_MAX_AGE_SECS, "3600");

    let registry = Registry::open_at(&db_path).expect("open registry");
    registry
        .insert_if_new(&InsertInput {
            kind: EXTERN_REPO_KIND.to_string(),
            path: repo.to_string_lossy().to_string(),
            repo_root: Some(dir.path().to_string_lossy().to_string()),
            branch: None,
            agent_id: None,
            now_unix: now_unix(),
        })
        .expect("insert extern repo");

    let live_cwds_provider: LiveCwdsProvider = Arc::new(Vec::<PathBuf>::new);
    let pool_tx = spawn_purge_pool(1);
    let (completion_tx, completion_rx) = mpsc::channel::<RegistryMsg>();
    run_periodic_purge_tick(&registry, &pool_tx, &completion_tx, &live_cwds_provider);
    let drained = drain_purge_completions(
        &registry,
        &completion_rx,
        Duration::from_millis(150),
        Duration::from_millis(500),
    );
    assert_eq!(drained, 0, "fresh extern-repo must not be dispatched");

    let rows = registry.list(Some(EXTERN_REPO_KIND)).expect("list");
    assert_eq!(rows.len(), 1, "fresh extern-repo row should survive");
    assert!(repo.exists(), "fresh extern-repo dir should survive");
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

/// Issue #268: the env-driven purge-pool concurrency knob.
#[test]
fn purge_concurrency_from_raw_picks_user_value_and_falls_back() {
    let default = default_purge_concurrency();
    assert!((1..=DEFAULT_GC_PURGE_CONCURRENCY_CAP).contains(&default));
    assert_eq!(purge_concurrency_from_raw(None), default);
    assert_eq!(purge_concurrency_from_raw(Some("4")), 4);
    // Empty / zero / non-numeric all fall back to the default.
    assert_eq!(purge_concurrency_from_raw(Some(" ")), default);
    assert_eq!(purge_concurrency_from_raw(Some("0")), default);
    assert_eq!(purge_concurrency_from_raw(Some("bad")), default);
}

/// Issue #268: dispatch returns `PurgeStarted` with the count of
/// jobs enqueued plus the count filtered out by the live/kind
/// gates, not the count actually removed.
#[test]
fn dispatch_purge_entries_returns_purge_started_with_counts() {
    let dir = tempfile::tempdir().unwrap();
    let path_keep = dir.path().join("keep");
    let path_a = dir.path().join("a");
    let path_b = dir.path().join("b");
    for p in [&path_keep, &path_a, &path_b] {
        std::fs::create_dir_all(p).unwrap();
    }
    let candidates = vec![
        TrackedEntry {
            id: 1,
            kind: "cache".to_string(),
            path: path_a.to_string_lossy().to_string(),
            repo_root: None,
            branch: None,
            agent_id: None,
            created_unix: 100,
        },
        TrackedEntry {
            id: 2,
            kind: "cache".to_string(),
            path: path_b.to_string_lossy().to_string(),
            repo_root: None,
            branch: None,
            agent_id: None,
            created_unix: 100,
        },
        // path_keep is "live" via the live-cwd filter below.
        TrackedEntry {
            id: 3,
            kind: "cache".to_string(),
            path: path_keep.to_string_lossy().to_string(),
            repo_root: None,
            branch: None,
            agent_id: None,
            created_unix: 100,
        },
    ];
    let pool_tx = spawn_purge_pool(2);
    let (completion_tx, _completion_rx) = mpsc::channel::<RegistryMsg>();
    let reply = dispatch_purge_entries(
        &pool_tx,
        &completion_tx,
        candidates,
        vec![path_keep.clone()],
    );
    match reply {
        GcReply::PurgeStarted {
            dispatched,
            skipped,
        } => {
            assert_eq!(dispatched, 2);
            assert_eq!(skipped, 1);
        }
        other => panic!("expected PurgeStarted, got {other:?}"),
    }
}
