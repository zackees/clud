use super::*;
use crate::gc::ENV_DATA_DB;
use std::ffi::OsString;
use std::fs;
use std::sync::Mutex;

#[path = "tests/parallel.rs"]
mod parallel;
#[path = "tests/policy.rs"]
mod policy;
use policy::{restore_env_var, spawn_test_worker};

// ENV_DATA_DB is process-global; serialize so two test threads
// never race to open the same redb file concurrently.
static TEST_LOCK: Mutex<()> = Mutex::new(());

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

/// How long a single worker round-trip may take before it is treated as slow.
/// The registry worker is single-threaded, so while it applies a bulk purge's
/// completions an unrelated `List` queues behind them.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// One request/response round-trip, returning `Err` on timeout rather than
/// panicking (issue #594).
///
/// The difference matters inside a polling loop: [`wait_for_row_count`] owns a
/// deadline of its own and is built to absorb a slow reply, but the previous
/// `recv_timeout(..).unwrap()` aborted the whole test on the first slow one, so
/// the outer deadline never got to do its job. That is how
/// `bulk_purge_keeps_serving_list_while_pool_grinds_through` failed on a loaded
/// Windows runner: 50 `remove_dir_all` calls kept the worker busy past five
/// seconds, and the panic carried no timing information at all.
fn try_call(
    tx: &mpsc::Sender<RegistryMsg>,
    op: GcOp,
    timeout: Duration,
) -> Result<GcReply, mpsc::RecvTimeoutError> {
    let (reply_tx, reply_rx) = mpsc::sync_channel::<GcReply>(1);
    tx.send(RegistryMsg::Op(GcRequestMsg { op, reply_tx }))
        .unwrap();
    reply_rx.recv_timeout(timeout)
}

/// One round-trip that must succeed. Use where a slow worker is itself the
/// bug; prefer [`try_call`] inside a loop that already has a deadline.
fn call(tx: &mpsc::Sender<RegistryMsg>, op: GcOp) -> GcReply {
    let started = Instant::now();
    match try_call(tx, op, CALL_TIMEOUT) {
        Ok(reply) => reply,
        Err(err) => panic!(
            "registry worker did not reply within {CALL_TIMEOUT:?} (waited {:?}): {err}",
            started.elapsed()
        ),
    }
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
    wait_for_row_count_with_budget(tx, kind, target_count, timeout, CALL_TIMEOUT)
}

/// [`wait_for_row_count`] with the per-round-trip budget exposed.
///
/// A round-trip that times out counts as "not yet" and is retried until the
/// caller's own deadline, which is the point of having one. On expiry the panic
/// names how long it waited and how many round-trips were slow, so the next
/// occurrence says whether the worker was merely busy or wedged — the question
/// #594 is trying to answer, and one the old panic destroyed the evidence for.
///
/// Only the tests for this helper pass anything but [`CALL_TIMEOUT`]: proving a
/// slow reply is absorbed needs a round-trip that actually exceeds its budget,
/// and stalling a worker for five seconds to arrange that would be five seconds
/// of dead test time.
fn wait_for_row_count_with_budget(
    tx: &mpsc::Sender<RegistryMsg>,
    kind: Option<&str>,
    target_count: usize,
    timeout: Duration,
    call_budget: Duration,
) -> Vec<ListRow> {
    let started = Instant::now();
    let deadline = started + timeout;
    let mut slow_calls = 0usize;
    let mut last_rows: Option<Vec<ListRow>> = None;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        // Never wait past the caller's deadline, and always leave the loop a
        // chance to observe expiry.
        let budget = call_budget.min(remaining.max(Duration::from_millis(1)));
        match try_call(
            tx,
            GcOp::List {
                kind: kind.map(String::from),
            },
            budget,
        ) {
            Ok(GcReply::ListOk { rows }) => {
                if rows.len() == target_count {
                    return rows;
                }
                last_rows = Some(rows);
            }
            Ok(other) => panic!("unexpected reply: {other:?}"),
            Err(_) => slow_calls += 1,
        }

        if Instant::now() >= deadline {
            return last_rows.unwrap_or_else(|| {
                panic!(
                    "worker never answered a List in {:?} ({slow_calls} round-trip(s) \
                     exceeded {call_budget:?}); expected {target_count} row(s)",
                    started.elapsed()
                )
            });
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Apply exactly `expected` purge completions from `rx`, or fail with the
/// shortfall.
///
/// Prefer this to [`drain_purge_completions`] whenever the dispatch count is
/// known. Issue #560: a quiet interval is not evidence that a purge finished —
/// two directory removals dispatched to a two-worker pool can land more than
/// any fixed gap apart under AV-scanner or `TempDir` contention on Windows,
/// and the drain would return after the first one with the test none the
/// wiser. Waiting for a count the *producer* reported turns that from a
/// timing guess into an assertion.
///
/// `timeout` remains a hard bound so a genuinely lost completion fails the
/// test rather than hanging it.
fn expect_purge_completions(
    registry: &Registry,
    rx: &mpsc::Receiver<RegistryMsg>,
    expected: usize,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    let mut applied = 0usize;
    let mut other_messages = 0usize;
    while applied < expected {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            panic!(
                "timed out after {timeout:?} waiting for purge completions: \
                 applied {applied} of {expected} dispatched \
                 ({other_messages} non-completion message(s) seen). \
                 A missing completion means a purge worker never reported back."
            );
        }
        match rx.recv_timeout(remaining) {
            Ok(RegistryMsg::PurgeCompletion(c)) => {
                apply_purge_completion(registry, c);
                applied += 1;
            }
            Ok(_) => other_messages += 1,
            Err(err) => panic!(
                "purge completion channel closed after {applied} of {expected}: {err} \
                 ({other_messages} non-completion message(s) seen)"
            ),
        }
    }
}

/// Drain `RegistryMsg::PurgeCompletion(..)` items from `rx` until
/// either no completion arrives within `quiet_for` or `timeout`
/// elapses, applying each one against `registry`. Used by tests
/// that drive the periodic-tick helpers directly — outside the
/// worker loop the test plays the role of the worker.
///
/// Only appropriate where the expected count is zero or genuinely unknown;
/// see [`expect_purge_completions`].
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
            Ok(RegistryMsg::WatchRescan(_)) => {
                // Watch notifications are irrelevant to periodic-purge tests.
            }
            Err(_) => return drained,
        }
    }
}

/// Issue #594: a worker that never answers must produce a panic that says so,
/// naming the elapsed time and the number of slow round-trips.
///
/// The receiver is held but never replied to, which is the wedged case. Before
/// this change the failure surfaced as a bare `RecvTimeoutError` unwrap inside
/// `call`, with no indication of which wait had expired or for how long — the
/// reason characterising the real occurrence needed a log dig.
#[test]
#[should_panic(expected = "worker never answered a List")]
fn a_wedged_worker_produces_a_diagnosable_timeout() {
    let (tx, _held_receiver) = mpsc::channel::<RegistryMsg>();
    // Short deadline: the point is the message, not the waiting.
    let _ = wait_for_row_count(&tx, None, 0, Duration::from_millis(50));
}

/// The complementary case, and the actual regression guard: a worker that is
/// merely *slow* must not abort the test. The first round-trip outlives its
/// per-call budget, the helper retries within its own deadline, and the answer
/// still arrives.
///
/// The budget is passed explicitly and is deliberately shorter than the stall.
/// With the production 5 s budget a 120 ms stall never expires, so the test
/// would pass against the old panicking code too and guard nothing — which is
/// the exact failure mode this change is about. Verified by re-introducing the
/// panic locally: this test fails, the wedged-worker one still passes.
#[test]
fn a_slow_reply_is_absorbed_by_the_callers_deadline() {
    let (tx, rx) = mpsc::channel::<RegistryMsg>();
    let worker = thread::spawn(move || {
        // Stall past the per-call budget, the way a real worker does while
        // grinding through a bulk purge. The caller abandons this first reply,
        // exactly as in production.
        let first = rx.recv().expect("a request");
        thread::sleep(Duration::from_millis(120));
        if let RegistryMsg::Op(req) = first {
            let _ = req.reply_tx.send(GcReply::ListOk { rows: Vec::new() });
        }
        // Answer every retry promptly.
        while let Ok(msg) = rx.recv() {
            if let RegistryMsg::Op(req) = msg {
                let _ = req.reply_tx.send(GcReply::ListOk { rows: Vec::new() });
            }
        }
    });

    let rows = wait_for_row_count_with_budget(
        &tx,
        None,
        0,
        Duration::from_secs(5),
        Duration::from_millis(30),
    );
    assert!(rows.is_empty());
    drop(tx);
    let _ = worker.join();
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
    assert!(matches!(resp, GcReply::InsertOk { inserted: true }));

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
    let dispatched = run_periodic_purge_tick_with_free_space(
        &registry,
        &pool_tx,
        &completion_tx,
        &live_cwds_provider,
        &config,
        &|_| Ok(4 * BYTES_PER_GB),
        PeriodicPurgeContext {
            session_state_dir: None,
            activity: None,
            spare_reasons: &mut SpareReasons::new(),
        },
    );
    // The two seeded entries — one worktree, one sibling clone — are both
    // old enough and both under a low-space root, so the tick must dispatch
    // exactly two jobs. Asserting this separately keeps the wait below
    // honest: without it, a tick that dispatched nothing would "satisfy"
    // zero completions and the teardown assertions would be vacuous.
    assert_eq!(dispatched, 2, "tick should dispatch both stale entries");

    // Outside the worker loop the test plays the role of the
    // registry-writer thread: apply the pool's completion callbacks to redb
    // directly. Issues #383/#560: this used to stop after a quiet gap
    // (250ms, then 1500ms), which is a guess about scheduler, AV-scanner and
    // TempDir latency rather than a fact about the purge. Wait for the count
    // the tick actually reported instead.
    expect_purge_completions(
        &registry,
        &completion_rx,
        dispatched,
        Duration::from_secs(30),
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
        PeriodicPurgeContext {
            session_state_dir: None,
            activity: None,
            spare_reasons: &mut SpareReasons::new(),
        },
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
    let dispatched = run_periodic_purge_tick(
        &registry,
        &pool_tx,
        &completion_tx,
        &live_cwds_provider,
        None,
        None,
        &mut SpareReasons::new(),
    );
    // Same #383/#560 hazard as the periodic-purge test: this one discarded
    // the drain count entirely, so a completion that never arrived showed up
    // only as the "dir should be deleted" assertion failing — with no hint
    // that the purge simply hadn't finished yet.
    assert_eq!(dispatched, 1, "the stale extern repo should be dispatched");
    expect_purge_completions(
        &registry,
        &completion_rx,
        dispatched,
        Duration::from_secs(30),
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
    run_periodic_purge_tick(
        &registry,
        &pool_tx,
        &completion_tx,
        &live_cwds_provider,
        None,
        None,
        &mut SpareReasons::new(),
    );
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

/// Issue #896, and the load-bearing test for its design: `gc list` must
/// explain a retained row *without* re-running the git probe, because it is
/// a hot client op on the registry worker thread (see #946).
///
/// The proof is the second assertion: the very same registry row reads as a
/// plain `reclaimable` row when handed an empty cache. If `List` probed git
/// itself it would reach the same `pinned` verdict both times, and that
/// assertion would fail.
#[test]
fn list_surfaces_the_ticks_reason_from_cache_and_never_probes_itself() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("list-state.redb");
    let repo = dir.path().join("extern-pinned");
    // A `.git` that is not a usable repository: the anchor gate passes, then
    // every probe query fails, which is `ProbeFailed` → spared. That is a
    // real pinned state and needs no git fixture to reproduce.
    fs::create_dir_all(repo.join(".git")).unwrap();

    // Idle immediately, so the tick evaluates rather than skipping on mtime.
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

    let pool_tx = spawn_purge_pool(1);
    let (completion_tx, _completion_rx) = mpsc::channel::<RegistryMsg>();
    let mut spare_reasons = SpareReasons::new();

    // A dry-run purge exercises the same evaluation the periodic tick does,
    // without deleting anything out from under the assertions.
    let purge_reply = process_op(
        &registry,
        &pool_tx,
        &completion_tx,
        GcOp::Purge {
            duration: None,
            kind: Some(EXTERN_REPO_KIND.to_string()),
            dry_run: true,
        },
        Vec::new(),
        &mut spare_reasons,
    );
    match purge_reply {
        GcReply::PurgeOk { removed, skipped } => {
            assert_eq!(removed, 0, "an unreadable checkout must not be purgeable");
            assert_eq!(skipped, 1, "it should be counted as spared");
        }
        other => panic!("expected PurgeOk, got {other:?}"),
    }
    assert!(
        !spare_reasons.is_empty(),
        "the evaluation must have recorded a verdict for gc list to reuse"
    );

    let rows = list_rows(&registry, &pool_tx, &completion_tx, &mut spare_reasons);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, "pinned");
    assert!(!rows[0].reclaimable);
    assert_eq!(
        rows[0].reason.as_deref(),
        Some("pinned: git state unreadable")
    );
    assert!(
        rows[0].evaluated_unix.is_some(),
        "a cached verdict should carry when it was computed"
    );

    // The same row, with nothing cached: plain reclaimable, no reason. This
    // is what proves the state came from the cache and not from `List`
    // probing git on its own.
    let uncached = list_rows(
        &registry,
        &pool_tx,
        &completion_tx,
        &mut SpareReasons::new(),
    );
    assert_eq!(uncached[0].state, "reclaimable");
    assert!(uncached[0].reclaimable);
    assert_eq!(uncached[0].reason, None);
    assert_eq!(uncached[0].evaluated_unix, None);
}

/// A registry row whose directory is gone reads as `dangling` (red) — and
/// that needs no cached verdict, since `List` can stat the path itself.
#[test]
fn list_marks_a_row_whose_path_is_gone_as_dangling() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("dangling.redb");

    let registry = Registry::open_at(&db_path).expect("open registry");
    registry
        .insert_if_new(&InsertInput {
            kind: WORKTREE_KIND.to_string(),
            path: dir
                .path()
                .join("never-existed")
                .to_string_lossy()
                .to_string(),
            repo_root: None,
            branch: None,
            agent_id: None,
            now_unix: now_unix(),
        })
        .expect("insert worktree");

    let pool_tx = spawn_purge_pool(1);
    let (completion_tx, _completion_rx) = mpsc::channel::<RegistryMsg>();
    let rows = list_rows(
        &registry,
        &pool_tx,
        &completion_tx,
        &mut SpareReasons::new(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, "dangling");
    assert!(!rows[0].reclaimable);
    assert_eq!(rows[0].reason.as_deref(), Some("dangling: path missing"));
}

fn list_rows(
    registry: &Registry,
    pool_tx: &mpsc::Sender<PurgeJob>,
    completion_tx: &mpsc::Sender<RegistryMsg>,
    spare_reasons: &mut SpareReasons,
) -> Vec<ListRow> {
    match process_op(
        registry,
        pool_tx,
        completion_tx,
        GcOp::List { kind: None },
        Vec::new(),
        spare_reasons,
    ) {
        GcReply::ListOk { rows } => rows,
        other => panic!("expected ListOk, got {other:?}"),
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

#[path = "tests/delete_and_pool.rs"]
mod delete_and_pool;
