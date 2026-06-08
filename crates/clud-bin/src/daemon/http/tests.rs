use super::*;
use crate::daemon::gc_service::spawn_registry_worker_with;
use crate::daemon::types::{CtrlCProfile, SessionKind, SessionSnapshot};
use crate::gc::Registry;
use std::io::Write;

fn write_fake_session(state_dir: &Path, id: &str, snap: SessionSnapshot) {
    let dir = state_dir.join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{id}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&snap).unwrap()).unwrap();
}

fn fake_snapshot(id: &str, name: &str, cwd: &str) -> SessionSnapshot {
    SessionSnapshot {
        id: id.to_string(),
        kind: SessionKind::Pty,
        cwd: Some(cwd.to_string()),
        name: Some(name.to_string()),
        created_at: Some(500),
        detachable: false,
        background: false,
        attachable: true,
        repeat_interval_secs: None,
        repeat_next_run_at: None,
        repeat_running: false,
        // Sensitive fields — the SessionView should drop these.
        daemon_pid: 1,
        // A PID this unlikely to be alive forces live=false in tests.
        worker_pid: 4_000_000_000,
        worker_port: 12345,
        root_pid: None,
        exit_code: None,
        ctrl_c: None,
    }
}

#[test]
fn dashboard_url_format() {
    assert_eq!(dashboard_url_from_info(54321), "http://127.0.0.1:54321/");
}

#[test]
fn purge_request_defaults_when_body_is_empty() {
    let parsed: PurgeRequest = serde_json::from_str("{}").unwrap();
    assert!(parsed.id.is_none());
    assert!(parsed.kind.is_none());
}

#[test]
fn purge_request_round_trips_kind_filter() {
    let json = r#"{"kind":"worktree"}"#;
    let parsed: PurgeRequest = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.kind.as_deref(), Some("worktree"));
}

#[test]
fn dashboard_html_asset_loads() {
    // Sanity check: the embedded asset compiled in. Tests pulled from
    // disk would mask a missing `include_str!`.
    assert!(DASHBOARD_HTML.contains("clud"));
}

#[test]
fn find_body_start_after_crlf_crlf() {
    let raw = b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{\"x\":1}";
    let idx = find_body_start(raw).unwrap();
    assert_eq!(&raw[idx..], b"{\"x\":1}");
}

/// Shared "no live sessions" provider for the tests below that pre-date
/// issue #190 — they don't care about the registry merge and would
/// otherwise have to fight the global `CLUD_SESSION_DB` env-var.
fn empty_live_provider() -> super::LiveSessionsProvider {
    std::sync::Arc::new(Vec::new)
}

#[test]
fn build_state_with_empty_state_dir_returns_zeros() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_dashboard_state(dir.path(), None, 9999, 100, Vec::new()).expect("build");
    assert_eq!(state.stats.session_count, 0);
    assert_eq!(state.stats.live_session_count, 0);
    assert_eq!(state.stats.gc_count, 0);
    assert_eq!(state.stats.repo_count, 0);
    assert_eq!(state.daemon.ipc_port, 9999);
    assert_eq!(state.daemon.started_at_unix, 100);
    assert_eq!(state.daemon.version, env!("CARGO_PKG_VERSION"));
}

/// Issue #190: direct-runner `clud` invocations only show up in the
/// redb session registry, not as JSON snapshots on disk. The dashboard
/// must merge those rows in so the Sessions tab isn't perpetually
/// empty for users who never use `--detach` / `--experimental-daemon-centralized`.
///
/// Inject a synthetic `LiveSession` directly so this test can run in
/// parallel with the rest of the suite — no env-var fiddling, no
/// cross-test races on `CLUD_SESSION_DB`.
#[test]
fn build_state_surfaces_direct_runner_registry_rows() {
    let dir = tempfile::tempdir().unwrap();
    let live = vec![LiveSession {
        pid: 4242,
        started_unix: 1_700_000_000,
        backend: Some("claude".to_string()),
        launch_mode: Some("subprocess".to_string()),
        cwd: Some("/dev/repo".to_string()),
    }];

    let state = build_dashboard_state(dir.path(), None, 9999, 100, live).expect("build");
    let direct: Vec<_> = state
        .sessions
        .iter()
        .filter(|s| s.kind == "direct")
        .collect();
    assert_eq!(
        direct.len(),
        1,
        "registry-backed direct session should appear; got {:?}",
        state.sessions
    );
    assert_eq!(direct[0].id, "direct-4242");
    assert_eq!(direct[0].name.as_deref(), Some("claude"));
    assert_eq!(direct[0].cwd.as_deref(), Some("/dev/repo"));
    assert!(direct[0].live);
    assert_eq!(direct[0].worker_port, 0);
    // The live-session count in the stats must include direct sessions
    // — that's what the dashboard header displays.
    assert_eq!(state.stats.live_session_count, 1);
}

#[test]
fn build_state_includes_session_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    write_fake_session(
        dir.path(),
        "sess-a",
        fake_snapshot("sess-a", "test", "/dev/foo"),
    );

    let state = build_dashboard_state(dir.path(), None, 9999, 100, Vec::new()).expect("build");
    assert_eq!(state.sessions.len(), 1);
    assert_eq!(state.sessions[0].id, "sess-a");
    assert_eq!(state.sessions[0].name.as_deref(), Some("test"));
    assert_eq!(state.sessions[0].cwd.as_deref(), Some("/dev/foo"));
    assert_eq!(state.sessions[0].kind, "pty");
    // Unlikely-PID worker should be reported as not live.
    assert!(!state.sessions[0].live);

    // SessionView must not expose `daemon_pid` / `worker_pid` / `root_pid`.
    let json = serde_json::to_value(&state.sessions[0]).unwrap();
    assert!(json.get("daemon_pid").is_none());
    assert!(json.get("worker_pid").is_none());
    assert!(json.get("root_pid").is_none());
}

#[test]
fn build_state_includes_ctrl_c_events_when_present() {
    use crate::ctrl_c_track::{events_dir, CtrlCEvent, InvocationKind};
    let dir = tempfile::tempdir().unwrap();
    let edir = events_dir(dir.path());
    std::fs::create_dir_all(&edir).unwrap();
    for i in 0..3u64 {
        let event = CtrlCEvent {
            pid: 1_000 + i as u32,
            observed_at_ms: 1_700_000_000_000 + i * 1000,
            exit_at_ms: 1_700_000_000_500 + i * 1000,
            elapsed_ms: 500 + i,
            kind: InvocationKind::Direct,
            exit_code: 130,
            cwd: Some(format!("/tmp/a{i}")),
            handed_off: Some(i % 2 == 0),
            handoff_reason: Some(if i % 2 == 0 {
                "ctrl_c_subprocess".to_string()
            } else {
                "daemon_unreachable".to_string()
            }),
        };
        let path = edir.join(format!("{:013}-{}.json", event.exit_at_ms, event.pid));
        std::fs::write(&path, serde_json::to_vec(&event).unwrap()).unwrap();
    }
    let state = build_dashboard_state(dir.path(), None, 9999, 100, Vec::new()).expect("build");
    assert_eq!(state.ctrl_c_events.len(), 3);
    // Newest first
    assert_eq!(state.ctrl_c_events[0].exit_at_ms, 1_700_000_000_500 + 2_000);
    assert_eq!(state.ctrl_c_events[2].exit_at_ms, 1_700_000_000_500);
}

#[test]
fn build_state_includes_ctrl_c_profile() {
    let dir = tempfile::tempdir().unwrap();
    let mut snap = fake_snapshot("sess-ctrl-c", "interrupt", "/dev/ctrl-c");
    snap.ctrl_c = Some(CtrlCProfile {
        cli_pid: Some(777),
        cli_observed_at_ms: Some(10_000),
        cli_handoff_at_ms: Some(10_025),
        cli_return_ready_at_ms: Some(10_025),
        cli_handoff_ms: Some(25),
        daemon_received_at_ms: Some(10_026),
        daemon_kill_started_at_ms: Some(10_026),
        daemon_kill_finished_at_ms: Some(10_090),
        daemon_kill_ms: Some(64),
        fast_path: true,
    });
    write_fake_session(dir.path(), "sess-ctrl-c", snap);

    let state = build_dashboard_state(dir.path(), None, 9999, 100, Vec::new()).expect("build");
    let profile = state.sessions[0].ctrl_c.as_ref().expect("profile");
    assert_eq!(profile.cli_handoff_ms, Some(25));
    assert_eq!(profile.daemon_kill_ms, Some(64));
    assert!(profile.fast_path);
}

#[test]
fn end_to_end_state_endpoint_returns_all_three_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("e2e.redb");

    // Seed: one session.
    write_fake_session(
        dir.path(),
        "sess-x",
        fake_snapshot("sess-x", "fix", "/dev/repo"),
    );

    // Seed: one GC row + one repo visit.
    let registry = Registry::open_at(&db_path).expect("open registry");
    let gc_tx = spawn_registry_worker_with(registry).expect("worker");
    let (rx_t, rx) = mpsc::sync_channel::<GcReply>(1);
    gc_tx
        .send(RegistryMsg::Op(GcRequestMsg {
            op: GcOp::Insert {
                kind: "worktree".to_string(),
                path: "/tmp/wt-x".to_string(),
                repo_root: Some("/dev/repo".to_string()),
                branch: Some("feat/x".to_string()),
                agent_id: Some("agent-x".to_string()),
                created_unix: Some(1000),
            },
            reply_tx: rx_t,
        }))
        .unwrap();
    let _ = rx.recv_timeout(Duration::from_secs(2)).unwrap();

    let (rx_t, rx) = mpsc::sync_channel::<GcReply>(1);
    gc_tx
        .send(RegistryMsg::Op(GcRequestMsg {
            op: GcOp::RecordRepoVisit {
                repo_root: "/dev/repo".to_string(),
                cwd: "/dev/repo".to_string(),
                now_unix: Some(2000),
            },
            reply_tx: rx_t,
        }))
        .unwrap();
    let _ = rx.recv_timeout(Duration::from_secs(2)).unwrap();

    // Spawn the actual HTTP server.
    let port = spawn_dashboard(
        dir.path().to_path_buf(),
        Some(gc_tx.clone()),
        9999,
        100,
        empty_live_provider(),
    )
    .expect("dashboard spawned");

    // Hit /state.json.
    let body = fetch_state_json(port).expect("fetch state");
    let state: DashboardState = serde_json::from_str(&body).expect("parse");
    assert_eq!(state.stats.session_count, 1);
    assert_eq!(state.stats.gc_count, 1);
    assert_eq!(state.stats.repo_count, 1);
    assert_eq!(state.sessions[0].name.as_deref(), Some("fix"));
    assert_eq!(state.gc[0].kind, "worktree");
    assert_eq!(state.repos[0].repo_root, "/dev/repo");
    assert_eq!(state.repos[0].run_count, 1);

    // Hit GET / and confirm the HTML asset is served.
    let html_body = fetch_path(port, "GET", "/", None).expect("fetch root");
    assert!(html_body.contains("clud dashboard"));
}

#[test]
fn end_to_end_purge_kind_round_trip_mutates_registry() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("purge.redb");

    let registry = Registry::open_at(&db_path).expect("open registry");
    let gc_tx = spawn_registry_worker_with(registry).expect("worker");
    for p in ["/tmp/p-a", "/tmp/p-b"] {
        let (rx_t, rx) = mpsc::sync_channel::<GcReply>(1);
        gc_tx
            .send(RegistryMsg::Op(GcRequestMsg {
                op: GcOp::Insert {
                    kind: "cache".to_string(),
                    path: p.to_string(),
                    repo_root: None,
                    branch: None,
                    agent_id: None,
                    created_unix: Some(1000),
                },
                reply_tx: rx_t,
            }))
            .unwrap();
        let _ = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    let port = spawn_dashboard(
        dir.path().to_path_buf(),
        Some(gc_tx.clone()),
        9999,
        100,
        empty_live_provider(),
    )
    .expect("dashboard spawned");

    // POST /gc/purge {"kind":"cache"} — bulk async purge.
    let body = fetch_path(
        port,
        "POST",
        "/gc/purge",
        Some(r#"{"kind":"cache"}"#.to_string()),
    )
    .expect("purge");
    let resp: PurgeResponse = serde_json::from_str(&body).expect("parse");
    // Issue #268: bulk purge replies `dispatched`, not `removed`.
    // The two entries point at /tmp/p-a and /tmp/p-b, which do not
    // exist on disk → `remove_dir_all` short-circuits to Ok and the
    // worker drops the redb rows once the completions land.
    assert_eq!(resp.dispatched, Some(2));
    assert_eq!(resp.removed, None);
    assert_eq!(resp.skipped, 0);

    // Async deletes land slightly after the HTTP response — poll
    // until the registry shrinks rather than racing against it.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let state_body = fetch_state_json(port).expect("re-fetch state");
        let state: DashboardState = serde_json::from_str(&state_body).expect("parse state");
        if state.stats.gc_count == 0 {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "registry never drained after bulk purge (gc_count={})",
                state.stats.gc_count
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Per-row Delete button on the dashboard: `POST /gc/purge {id: N}`
/// must remove exactly the targeted row even when other rows share
/// its `kind`. Replaces the earlier "single row of a kind" workaround
/// that returned a 500 in this case.
#[test]
fn end_to_end_per_row_delete_only_targets_requested_id() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("delete-by-id.redb");

    let registry = Registry::open_at(&db_path).expect("open registry");
    let gc_tx = spawn_registry_worker_with(registry).expect("worker");

    // Three siblings of the same kind in a tempdir.
    let paths: Vec<String> = ["e1", "e2", "e3"]
        .iter()
        .map(|name| {
            let p = dir.path().join(name);
            std::fs::create_dir_all(&p).unwrap();
            p.to_string_lossy().to_string()
        })
        .collect();
    for p in &paths {
        let (rx_t, rx) = mpsc::sync_channel::<GcReply>(1);
        gc_tx
            .send(RegistryMsg::Op(GcRequestMsg {
                op: GcOp::Insert {
                    kind: "cache".to_string(),
                    path: p.clone(),
                    repo_root: None,
                    branch: None,
                    agent_id: None,
                    created_unix: Some(1000),
                },
                reply_tx: rx_t,
            }))
            .unwrap();
        let _ = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    let port = spawn_dashboard(
        dir.path().to_path_buf(),
        Some(gc_tx.clone()),
        9999,
        100,
        empty_live_provider(),
    )
    .expect("dashboard spawned");

    // Fetch /state.json to get the assigned ids.
    let state_body = fetch_state_json(port).expect("fetch state");
    let state: DashboardState = serde_json::from_str(&state_body).expect("parse");
    let middle = state
        .gc
        .iter()
        .find(|r| r.path == paths[1])
        .expect("middle row");

    // POST /gc/purge {"id": <middle.id>}
    let body = fetch_path(
        port,
        "POST",
        "/gc/purge",
        Some(format!(r#"{{"id":{}}}"#, middle.id)),
    )
    .expect("delete");
    let resp: PurgeResponse = serde_json::from_str(&body).expect("parse");
    // Per-row Delete uses the synchronous `DeleteById` path —
    // response shape stays `removed`, not `dispatched`.
    assert_eq!(resp.removed, Some(1));
    assert_eq!(resp.dispatched, None);
    assert_eq!(resp.skipped, 0);

    // The two siblings must survive.
    let after = fetch_state_json(port).expect("re-fetch state");
    let after_state: DashboardState = serde_json::from_str(&after).expect("parse");
    let surviving: Vec<&str> = after_state.gc.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(after_state.gc.len(), 2);
    assert!(surviving.contains(&paths[0].as_str()));
    assert!(surviving.contains(&paths[2].as_str()));
    assert!(!surviving.contains(&paths[1].as_str()));

    // On-disk deletion happened for the targeted row only.
    assert!(!std::path::Path::new(&paths[1]).exists());
    assert!(std::path::Path::new(&paths[0]).exists());
    assert!(std::path::Path::new(&paths[2]).exists());
}

/// Tiny HTTP/1.0 client for tests. Connect, send a request, read the
/// body. Avoids pulling in a real HTTP client dep just for tests.
fn fetch_path(port: u16, method: &str, path: &str, body: Option<String>) -> io::Result<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut req = format!("{method} {path} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n",);
    if let Some(b) = &body {
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            b.len(),
            b
        ));
    } else {
        req.push_str("\r\n");
    }
    stream.write_all(req.as_bytes())?;
    stream.flush()?;
    let mut buf = Vec::with_capacity(4096);
    stream.read_to_end(&mut buf)?;
    let body_start = find_body_start(&buf)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no header terminator"))?;
    String::from_utf8(buf[body_start..].to_vec())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}
