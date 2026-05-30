use super::*;

fn fresh_db_path(tag: &str) -> PathBuf {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(format!("data-{tag}.redb"));
    std::mem::forget(dir);
    path
}

fn fresh_registry(tag: &str) -> Registry {
    let path = fresh_db_path(tag);
    Registry::open_at(&path).expect("open registry")
}

fn insert(reg: &Registry, kind: &str, path: &str, now: i64) {
    reg.insert_if_new(&InsertInput {
        kind: kind.to_string(),
        path: path.to_string(),
        repo_root: None,
        branch: None,
        agent_id: None,
        now_unix: now,
    })
    .expect("insert");
}

#[test]
fn schema_bootstraps_on_first_open() {
    let path = fresh_db_path("bootstrap");
    let _r1 = Registry::open_at(&path).expect("first open");
    drop(_r1);
    let _r2 = Registry::open_at(&path).expect("reopen");
    // Reopening on a populated db must not error out.
}

// ---------- Issue #183: repo_visits table ----------

#[test]
fn record_repo_visit_inserts_first_row() {
    let reg = fresh_registry("repo-visit-insert");
    reg.record_repo_visit("/dev/foo", "/dev/foo", 1000)
        .expect("record");
    let rows = reg.list_repo_visits().expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].repo_root, "/dev/foo");
    assert_eq!(rows[0].last_cwd, "/dev/foo");
    assert_eq!(rows[0].last_visited_unix, 1000);
    assert_eq!(rows[0].run_count, 1);
}

#[test]
fn record_repo_visit_increments_run_count() {
    let reg = fresh_registry("repo-visit-incr");
    reg.record_repo_visit("/dev/foo", "/dev/foo", 1000).unwrap();
    reg.record_repo_visit("/dev/foo", "/dev/foo/sub", 1100)
        .unwrap();
    reg.record_repo_visit("/dev/foo", "/dev/foo", 1200).unwrap();
    let rows = reg.list_repo_visits().expect("list");
    assert_eq!(rows.len(), 1, "second/third visits upsert, never duplicate");
    assert_eq!(rows[0].run_count, 3);
    // last_visited and last_cwd reflect the most recent call.
    assert_eq!(rows[0].last_visited_unix, 1200);
    assert_eq!(rows[0].last_cwd, "/dev/foo");
}

#[test]
fn list_repo_visits_orders_newest_first() {
    let reg = fresh_registry("repo-visit-order");
    reg.record_repo_visit("/dev/old", "/dev/old", 100).unwrap();
    reg.record_repo_visit("/dev/newest", "/dev/newest", 9000)
        .unwrap();
    reg.record_repo_visit("/dev/mid", "/dev/mid", 500).unwrap();
    let rows = reg.list_repo_visits().expect("list");
    let repos: Vec<&str> = rows.iter().map(|r| r.repo_root.as_str()).collect();
    assert_eq!(repos, vec!["/dev/newest", "/dev/mid", "/dev/old"]);
}

#[test]
fn record_repo_visit_distinguishes_keys() {
    let reg = fresh_registry("repo-visit-distinct");
    reg.record_repo_visit("/dev/a", "/dev/a", 100).unwrap();
    reg.record_repo_visit("/dev/b", "/dev/b", 200).unwrap();
    let rows = reg.list_repo_visits().expect("list");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.run_count == 1));
}

#[test]
fn insert_then_list_round_trips() {
    let reg = fresh_registry("rt");
    insert(&reg, "worktree", "/tmp/a", 100);
    insert(&reg, "worktree", "/tmp/b", 200);
    let rows = reg.list(None).expect("list");
    assert_eq!(rows.len(), 2);
    // ORDER BY created_unix DESC → /tmp/b first.
    assert_eq!(rows[0].path, "/tmp/b");
    assert_eq!(rows[1].path, "/tmp/a");
}

#[test]
fn insert_if_new_is_noop_on_existing() {
    // The scanner-behavior contract: a second call on the same
    // (kind, path) leaves the original row untouched. The original
    // `created_unix` must survive, and no field is updated.
    let reg = fresh_registry("noop-existing");
    insert(&reg, "worktree", "/tmp/a", 100);
    let before = reg.list(None).unwrap()[0].clone();
    // Second insert with a later timestamp must be a no-op.
    reg.insert_if_new(&InsertInput {
        kind: "worktree".to_string(),
        path: "/tmp/a".to_string(),
        repo_root: Some("/repo".to_string()),
        branch: Some("main".to_string()),
        agent_id: Some("agent-x".to_string()),
        now_unix: 500,
    })
    .unwrap();
    let after = reg.list(None).unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].created_unix, 100, "created_unix must not change");
    assert_eq!(after[0].repo_root, before.repo_root);
    assert_eq!(after[0].branch, before.branch);
    assert_eq!(after[0].agent_id, before.agent_id);
    assert_eq!(
        after[0].id, before.id,
        "id must be stable across re-inserts"
    );
}

#[test]
fn purge_respects_kind_filter() {
    let reg = fresh_registry("kind-filter");
    insert(&reg, "worktree", "/tmp/wt-1", 100);
    insert(&reg, "worktree", "/tmp/wt-2", 100);
    insert(&reg, "cache", "/tmp/cache-1", 100);
    let cutoff = 500;
    // Filter to worktrees only.
    let older = reg.select_older_than(cutoff, Some("worktree")).unwrap();
    assert_eq!(older.len(), 2);
    assert!(older.iter().all(|r| r.kind == "worktree"));
    // No filter: all 3.
    let older_all = reg.select_older_than(cutoff, None).unwrap();
    assert_eq!(older_all.len(), 3);
}

#[test]
fn delete_removes_one_row() {
    let reg = fresh_registry("delete");
    insert(&reg, "worktree", "/tmp/a", 100);
    insert(&reg, "worktree", "/tmp/b", 100);
    let rows = reg.list(None).unwrap();
    assert_eq!(rows.len(), 2);
    reg.delete(rows[0].id).unwrap();
    assert_eq!(reg.count().unwrap(), 1);
}

#[test]
fn delete_on_missing_id_is_noop() {
    // The redb-backed delete scans for a matching id and silently
    // succeeds if nothing matches. This is intentional — `gc purge`
    // can fire the delete after the row has already been removed by
    // a concurrent operation, and we don't want to error out.
    let reg = fresh_registry("delete-missing");
    insert(&reg, "worktree", "/tmp/a", 100);
    // Try to delete a never-issued id.
    reg.delete(9999).unwrap();
    assert_eq!(reg.count().unwrap(), 1);
}

#[test]
fn ids_are_monotonic_across_inserts() {
    // The id counter is stored in the META table. Insert several rows
    // and confirm the ids strictly increase. (`gc purge` references
    // rows by id, so this needs to be stable.)
    let reg = fresh_registry("ids-mono");
    insert(&reg, "worktree", "/tmp/a", 100);
    insert(&reg, "worktree", "/tmp/b", 100);
    insert(&reg, "worktree", "/tmp/c", 100);
    let rows = reg.list(None).unwrap();
    let mut ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    ids.sort();
    // ids must be strictly increasing and distinct.
    for w in ids.windows(2) {
        assert!(w[0] < w[1], "ids must be strictly increasing: {:?}", ids);
    }
}

#[test]
fn extract_pid_parses_claude_format() {
    assert_eq!(
        extract_pid_from_lock_reason("claude agent agent-abf (pid 12345)"),
        Some(12345)
    );
}

#[test]
fn extract_pid_handles_no_match() {
    assert_eq!(extract_pid_from_lock_reason("manual lock by user"), None);
    assert_eq!(extract_pid_from_lock_reason("pid "), None);
    assert_eq!(extract_pid_from_lock_reason("pid abc"), None);
}

#[test]
fn extract_pid_handles_trailing_text() {
    assert_eq!(
        extract_pid_from_lock_reason("agent (pid 999) running"),
        Some(999)
    );
}

#[test]
fn reconcile_dir_inserts_agent_subdirs() {
    let reg = fresh_registry("reconcile-dir");
    let dir = tempfile::tempdir().unwrap();
    let watch = dir.path().to_path_buf();
    std::fs::create_dir_all(watch.join("agent-abc")).unwrap();
    std::fs::create_dir_all(watch.join("agent-def")).unwrap();
    // Non-agent dir is ignored.
    std::fs::create_dir_all(watch.join("not-an-agent")).unwrap();
    // File at top level is ignored.
    std::fs::write(watch.join("README"), b"hi").unwrap();
    let res = reconcile_dir(&reg, &watch, None).unwrap();
    assert_eq!(res.inserted, 2);
    assert_eq!(res.skipped, 0);
    let rows = reg.list(None).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.kind == "worktree"));
    assert!(rows.iter().all(|r| r
        .agent_id
        .as_deref()
        .map(|a| a.starts_with("agent-"))
        .unwrap_or(false)));
}

#[test]
fn reconcile_dir_handles_missing_watch_dir() {
    let reg = fresh_registry("reconcile-missing");
    let dir = tempfile::tempdir().unwrap();
    let watch = dir.path().join("does-not-exist");
    let res = reconcile_dir(&reg, &watch, None).unwrap();
    assert_eq!(res.inserted, 0);
    assert_eq!(reg.count().unwrap(), 0);
}

#[test]
fn reconcile_dir_is_idempotent_and_does_not_churn() {
    // Two passes over the same directory: the second pass must report
    // 0 inserted / 1 skipped, and the row's `created_unix` must
    // *not* change between passes — the new scanner contract is
    // "insert once, leave alone".
    let reg = fresh_registry("reconcile-idem");
    let dir = tempfile::tempdir().unwrap();
    let watch = dir.path().to_path_buf();
    std::fs::create_dir_all(watch.join("agent-abc")).unwrap();
    let first = reconcile_dir(&reg, &watch, None).unwrap();
    assert_eq!(first.inserted, 1);
    assert_eq!(first.skipped, 0);
    let before = reg.list(None).unwrap()[0].clone();
    // Sleep so that now_unix() would advance — if anything *did*
    // write the row, we'd notice via a changed created_unix.
    std::thread::sleep(Duration::from_millis(1100));
    let second = reconcile_dir(&reg, &watch, None).unwrap();
    assert_eq!(second.inserted, 0);
    assert_eq!(second.skipped, 1);
    let after = reg.list(None).unwrap()[0].clone();
    assert_eq!(
        after.created_unix, before.created_unix,
        "second pass must not modify the existing row"
    );
    assert_eq!(after.id, before.id, "id must remain stable");
}

// Issue #135: the scanner now talks IPC to the always-on clud daemon
// rather than opening redb directly. Verifying the end-to-end insert
// path lives in `daemon/gc_service.rs::tests` and the Python
// integration tests; here we only verify the scanner cancels promptly.

#[test]
fn scanner_cancels_promptly() {
    // Force the IPC path to be disabled so the scanner doesn't
    // attempt a real daemon spawn in CI.
    std::env::set_var(crate::daemon::ENV_NO_DAEMON, "1");
    let dir = tempfile::tempdir().unwrap();
    let mut scanner = WorktreeScanner::spawn(dir.path().to_path_buf(), None);
    let start = std::time::Instant::now();
    scanner.cancel();
    let elapsed = start.elapsed();
    // The chunked sleep wakes every 100ms; cancellation should be
    // observed well before one full 2s scan cycle. Allow some slack
    // for slow CI runners.
    assert!(
        elapsed < Duration::from_secs(1),
        "cancel took too long: {elapsed:?}"
    );
}
