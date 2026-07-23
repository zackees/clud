use std::cell::Cell;
use std::collections::HashSet;

use super::reconcile::ScanKind;
use super::scanner::{scan_once_with, ScanDeps};
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
fn insert_if_new_returns_true_then_false() {
    let reg = fresh_registry("inserted-flag");
    let input = InsertInput {
        kind: "worktree".to_string(),
        path: "/tmp/flag".to_string(),
        repo_root: None,
        branch: Some("first".to_string()),
        agent_id: None,
        now_unix: 100,
    };
    assert!(reg.insert_if_new(&input).unwrap());
    assert_eq!(reg.insert_write_transactions(), 1);
    assert!(!reg.insert_if_new(&input).unwrap());
    assert_eq!(reg.insert_write_transactions(), 1);
    assert_eq!(reg.list(None).unwrap()[0].branch.as_deref(), Some("first"));
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
    // succeeds if nothing matches. This is intentional: daemon-side
    // purge completion can fire the delete after the row has already
    // been removed by a concurrent operation, and we don't want to
    // error out.
    let reg = fresh_registry("delete-missing");
    insert(&reg, "worktree", "/tmp/a", 100);
    // Try to delete a never-issued id.
    reg.delete(9999).unwrap();
    assert_eq!(reg.count().unwrap(), 1);
}

#[test]
fn ids_are_monotonic_across_inserts() {
    // The id counter is stored in the META table. Insert several rows
    // and confirm the ids strictly increase. Daemon delete paths
    // reference rows by id, so this needs to be stable.
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
fn reconcile_extern_repos_dir_inserts_immediate_child_dirs() {
    let reg = fresh_registry("reconcile-extern-repos");
    let dir = tempfile::tempdir().unwrap();
    let repo_root = dir.path().join("repo");
    let watch = repo_root.join(".extern-repos");
    std::fs::create_dir_all(watch.join("dep-a")).unwrap();
    std::fs::create_dir_all(watch.join("dep-b").join("nested")).unwrap();
    std::fs::write(watch.join("README"), b"hi").unwrap();

    let res = reconcile_extern_repos_dir(&reg, &watch, Some(&repo_root)).unwrap();
    assert_eq!(res.inserted, 2);
    assert_eq!(res.skipped, 0);

    let rows = reg.list(None).unwrap();
    let repo_root_str = repo_root.to_string_lossy().to_string();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.kind == EXTERN_REPO_KIND));
    assert!(rows.iter().all(|r| r.agent_id.is_none()));
    assert!(rows
        .iter()
        .all(|r| r.repo_root.as_deref() == Some(repo_root_str.as_str())));
    assert!(rows.iter().any(|r| r.path.ends_with("dep-a")));
    assert!(rows.iter().any(|r| r.path.ends_with("dep-b")));
    assert!(!rows.iter().any(|r| r.path.ends_with("nested")));
}

#[test]
fn sibling_clone_name_matching_is_conservative() {
    for name in [
        "clud-pr-178",
        "clud-release-v1",
        "clud-issue-178",
        "clud-wt-fix",
        "soldr-wt-task",
        "zccache-wt-task",
    ] {
        assert!(
            is_sibling_clone_dir_name("clud", name),
            "expected {name} to match"
        );
    }

    for name in ["myrepo-wt-fix", "myrepo-issue-178"] {
        assert!(
            is_sibling_clone_dir_name("myrepo", name),
            "expected repo-scoped {name} to match"
        );
    }

    for name in [
        "clud",
        "agent-abc",
        "dep-a",
        "random-wt-task",
        "random-issue-178",
        "myrepo-pr-178",
        "clud-pr-",
        "soldr-wt-",
        "zccache-wt-",
    ] {
        assert!(
            !is_sibling_clone_dir_name("clud", name),
            "expected {name} to be rejected"
        );
    }
}

#[test]
fn reconcile_sibling_clones_dir_inserts_only_matching_siblings() {
    let reg = fresh_registry("reconcile-sibling-clones");
    let dir = tempfile::tempdir().unwrap();
    let repo_root = dir.path().join("clud");
    std::fs::create_dir_all(&repo_root).unwrap();

    for name in [
        "clud-wt-fix",
        "clud-issue-178",
        "clud-pr-178",
        "soldr-wt-task",
        "zccache-wt-task",
    ] {
        std::fs::create_dir_all(dir.path().join(name)).unwrap();
    }
    std::fs::create_dir_all(dir.path().join("random-wt-task")).unwrap();
    std::fs::create_dir_all(dir.path().join("random-issue-178")).unwrap();
    std::fs::write(dir.path().join("clud-release-file"), b"hi").unwrap();
    std::fs::create_dir_all(dir.path().join("clud-release-v1").join("nested")).unwrap();

    let res = reconcile_sibling_clones_dir(&reg, &repo_root).unwrap();
    assert_eq!(res.inserted, 6);
    assert_eq!(res.skipped, 0);

    let rows = reg.list(None).unwrap();
    let repo_root_str = repo_root.to_string_lossy().to_string();
    assert_eq!(rows.len(), 6);
    assert!(rows.iter().all(|r| r.kind == SIBLING_CLONE_KIND));
    assert!(rows.iter().all(|r| r.agent_id.is_none()));
    assert!(rows
        .iter()
        .all(|r| r.repo_root.as_deref() == Some(repo_root_str.as_str())));
    assert!(rows.iter().any(|r| r.path.ends_with("clud-wt-fix")));
    assert!(rows.iter().any(|r| r.path.ends_with("clud-issue-178")));
    assert!(rows.iter().any(|r| r.path.ends_with("clud-pr-178")));
    assert!(rows.iter().any(|r| r.path.ends_with("clud-release-v1")));
    assert!(rows.iter().any(|r| r.path.ends_with("soldr-wt-task")));
    assert!(rows.iter().any(|r| r.path.ends_with("zccache-wt-task")));
    assert!(!rows.iter().any(|r| r.path.ends_with("random-wt-task")));
    assert!(!rows.iter().any(|r| r.path.ends_with("random-issue-178")));
    assert!(!rows.iter().any(|r| r.path.ends_with("nested")));
    assert!(!rows.iter().any(|r| r.path.ends_with("clud")));
}

#[test]
fn reconcile_repo_root_counts_sibling_clones() {
    let reg = fresh_registry("reconcile-repo-root-count");
    let dir = tempfile::tempdir().unwrap();
    let repo_root = dir.path().join("repo");
    std::fs::create_dir_all(repo_root.join(".claude").join("worktrees").join("agent-a")).unwrap();
    std::fs::create_dir_all(repo_root.join(".extern-repos").join("dep-a")).unwrap();
    std::fs::create_dir_all(dir.path().join("repo-wt-fix")).unwrap();

    let inserted = reconcile_repo_root(&reg, &repo_root).unwrap();
    assert_eq!(inserted, 3);

    let rows = reg.list(None).unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().any(|r| r.kind == WORKTREE_KIND));
    assert!(rows.iter().any(|r| r.kind == EXTERN_REPO_KIND));
    assert!(rows.iter().any(|r| r.kind == SIBLING_CLONE_KIND));
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

#[test]
fn scan_skips_known_paths_after_first_success() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("agent-x")).unwrap();
    let mut seen = HashSet::new();
    let inserts = Cell::new(0);
    let branches = Cell::new(0);
    let mut insert = |_input: &InsertInput| {
        inserts.set(inserts.get() + 1);
        Ok(())
    };
    let branch_of = |_path: &std::path::Path| {
        branches.set(branches.get() + 1);
        Some("main".to_string())
    };
    let mut deps = ScanDeps {
        insert: &mut insert,
        branch_of: &branch_of,
    };
    scan_once_with(dir.path(), None, ScanKind::Worktree, &mut seen, &mut deps).unwrap();
    scan_once_with(dir.path(), None, ScanKind::Worktree, &mut seen, &mut deps).unwrap();
    assert_eq!(inserts.get(), 1);
    assert_eq!(branches.get(), 1);
}

#[test]
fn scan_does_not_memoize_failed_inserts() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("agent-x")).unwrap();
    let mut seen = HashSet::new();
    let inserts = Cell::new(0);
    let mut insert = |_input: &InsertInput| {
        inserts.set(inserts.get() + 1);
        if inserts.get() == 1 {
            Err("daemon unavailable".to_string())
        } else {
            Ok(())
        }
    };
    let branch_of = |_path: &std::path::Path| Some("main".to_string());
    let mut deps = ScanDeps {
        insert: &mut insert,
        branch_of: &branch_of,
    };
    assert!(scan_once_with(dir.path(), None, ScanKind::Worktree, &mut seen, &mut deps).is_err());
    assert!(seen.is_empty());
    scan_once_with(dir.path(), None, ScanKind::Worktree, &mut seen, &mut deps).unwrap();
    assert_eq!(inserts.get(), 2);
    assert_eq!(seen.len(), 1);
}

#[test]
fn scan_discovers_new_dir_with_warm_memo() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("agent-x")).unwrap();
    let mut seen = HashSet::new();
    let inserts = Cell::new(0);
    let mut insert = |_input: &InsertInput| {
        inserts.set(inserts.get() + 1);
        Ok(())
    };
    let branch_of = |_path: &std::path::Path| Some("main".to_string());
    let mut deps = ScanDeps {
        insert: &mut insert,
        branch_of: &branch_of,
    };
    scan_once_with(dir.path(), None, ScanKind::Worktree, &mut seen, &mut deps).unwrap();
    std::fs::create_dir_all(dir.path().join("agent-y")).unwrap();
    scan_once_with(dir.path(), None, ScanKind::Worktree, &mut seen, &mut deps).unwrap();
    assert_eq!(inserts.get(), 2);
    assert_eq!(seen.len(), 2);
}

#[test]
fn scan_defers_branch_lookup_until_after_memo_check() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("agent-x")).unwrap();
    let mut seen = HashSet::new();
    let branches = Cell::new(0);
    let mut insert = |_input: &InsertInput| Ok(());
    let branch_of = |_path: &std::path::Path| {
        branches.set(branches.get() + 1);
        Some("main".to_string())
    };
    let mut deps = ScanDeps {
        insert: &mut insert,
        branch_of: &branch_of,
    };
    scan_once_with(dir.path(), None, ScanKind::Worktree, &mut seen, &mut deps).unwrap();
    branches.set(0);
    scan_once_with(dir.path(), None, ScanKind::Worktree, &mut seen, &mut deps).unwrap();
    assert_eq!(branches.get(), 0);
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
