use super::*;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

/// Issue #192: a daemon whose `daemon.json` reports the same version
/// as the spawning binary must NOT be restarted. This is the steady-
/// state case for every `ensure_daemon` call after the first launch.
#[test]
fn daemon_version_matches_current_binary() {
    let info = DaemonInfo {
        pid: 1,
        pid_start: 0,
        port: 0,
        dashboard_port: None,
        dashboard_token: None,
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
    };
    assert!(daemon_version_matches(&info));
}

/// A daemon whose `daemon.json` reports a different version is stale
/// (likely a leftover from an in-place upgrade). `ensure_daemon` must
/// see this as a mismatch so the upgrade path replaces it.
#[test]
fn daemon_version_mismatch_when_versions_differ() {
    let info = DaemonInfo {
        pid: 1,
        pid_start: 0,
        port: 0,
        dashboard_port: None,
        dashboard_token: None,
        version: Some("0.0.0-not-the-current".to_string()),
    };
    assert!(!daemon_version_matches(&info));
}

/// `daemon.json` files written by clud <= 2.0.14 omit the `version`
/// field entirely. Treat them as stale so they're swept away on the
/// next launch — those daemons predate the registry-merge dashboard
/// fix (#190) and would keep reporting zero sessions.
#[test]
fn daemon_version_mismatch_when_field_absent() {
    let info = DaemonInfo {
        pid: 1,
        pid_start: 0,
        port: 0,
        dashboard_port: None,
        dashboard_token: None,
        version: None,
    };
    assert!(!daemon_version_matches(&info));
}

fn version_newer_than_current() -> String {
    let major = env!("CARGO_PKG_VERSION")
        .split('.')
        .next()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    format!("{}.0.0", major + 1)
}

#[test]
fn newer_daemon_is_refused_and_rendered_yellow() {
    let tmp = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let info = DaemonInfo {
        pid: std::process::id(),
        pid_start: crate::process_identity::self_start_time(),
        port,
        dashboard_port: None,
        dashboard_token: None,
        version: Some(version_newer_than_current()),
    };
    super::super::io_helpers::write_json_file(&daemon_info_path(tmp.path()), &info).unwrap();

    let error = ensure_daemon(tmp.path()).expect_err("older clud must refuse a newer daemon");
    assert!(is_incompatible_daemon_error(&error));
    let rendered = incompatible_daemon_error_line(&error);
    assert!(rendered.starts_with("\x1b[33m[clud] error:"));
    assert!(rendered.ends_with("\x1b[0m"));
    assert!(daemon_info_path(tmp.path()).exists());
}

#[test]
fn daemon_stop_preflight_refuses_newer_daemon_without_signaling_it() {
    let tmp = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let info = DaemonInfo {
        pid: std::process::id(),
        pid_start: crate::process_identity::self_start_time(),
        port,
        dashboard_port: None,
        dashboard_token: None,
        version: Some(version_newer_than_current()),
    };
    super::super::io_helpers::write_json_file(&daemon_info_path(tmp.path()), &info).unwrap();

    let error = request_daemon_shutdown(tmp.path())
        .expect_err("older clud must not send shutdown to a newer daemon");
    assert!(is_incompatible_daemon_error(&error));
    assert!(identity_is_alive(&info.identity()));
    assert!(daemon_info_path(tmp.path()).exists());
}

fn write_daemon_info(state_dir: &Path, pid: u32, port: u16) {
    fs::create_dir_all(state_dir).unwrap();
    let info = DaemonInfo {
        pid,
        pid_start: crate::process_identity::start_time_of(pid),
        port,
        dashboard_port: None,
        dashboard_token: None,
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
    };
    super::super::io_helpers::write_json_file(&daemon_info_path(state_dir), &info).unwrap();
}

fn write_unfinished_session(state_dir: &Path, id: &str) {
    fs::create_dir_all(sessions_dir(state_dir)).unwrap();
    let session = SessionSnapshot {
        id: id.to_string(),
        kind: super::super::types::SessionKind::Subprocess,
        backend: Some("codex".to_string()),
        launch_mode: Some("subprocess".to_string()),
        repo_root: None,
        command: vec!["codex".to_string()],
        cwd: None,
        name: None,
        created_at: Some(1),
        detachable: false,
        background: false,
        attachable: false,
        repeat_interval_secs: None,
        repeat_next_run_at: None,
        repeat_running: false,
        daemon_pid: 1,
        worker_pid: u32::MAX,
        worker_port: 0,
        root_pid: None,
        daemon_pid_start: 0,
        worker_pid_start: 0,
        root_pid_start: 0,
        exit_code: None,
        exited_at: None,
        ctrl_c: None,
    };
    super::super::io_helpers::write_json_file(&session_snapshot_path(state_dir, id), &session)
        .unwrap();
}

fn spawn_silent_peer() -> (u16, Arc<AtomicBool>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let saw_request = Arc::new(AtomicBool::new(false));
    let saw_request_thread = Arc::clone(&saw_request);

    thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            if !line.is_empty() {
                saw_request_thread.store(true, Ordering::SeqCst);
            }
        }
    });

    (port, saw_request)
}

#[test]
fn try_register_gc_watch_is_silent_when_daemon_is_unreachable() {
    let state = tempfile::tempdir().unwrap();
    let roots = [GcWatchRoot {
        kind: "worktree".to_string(),
        watch_dir: state.path().join("worktrees").to_string_lossy().to_string(),
        repo_root: None,
    }];
    assert!(!try_register_gc_watch(state.path(), &roots));
}

fn spawn_shutdown_ack_peer() -> (u16, mpsc::Receiver<String>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (line_tx, line_rx) = mpsc::channel();

    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let _ = line_tx.send(line.clone());
            let (_, format) = super::super::wire_prost::decode_daemon_request_line(&line).unwrap();
            let response = DaemonResponse::ShutdownAck { pid: 4242 };
            let bytes =
                super::super::wire_prost::encode_daemon_response_line(&response, format).unwrap();
            stream.write_all(&bytes).unwrap();
            stream.flush().unwrap();
        }
    });

    (port, line_rx)
}

#[test]
fn send_daemon_request_translates_silent_peer_to_unexpected_eof() {
    let tmp = tempfile::tempdir().unwrap();
    let (port, saw_request) = spawn_silent_peer();
    write_daemon_info(tmp.path(), std::process::id(), port);

    let err = send_daemon_request(
        tmp.path(),
        &DaemonRequest::Shutdown {
            client_version: None,
            expected_daemon: None,
        },
    )
    .expect_err("silent peer must not produce a daemon response");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    assert!(
        !err.to_string().contains("EOF while parsing a value"),
        "must not surface the raw serde_json EOF message: {err}"
    );

    for _ in 0..20 {
        if saw_request.load(Ordering::SeqCst) {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        saw_request.load(Ordering::SeqCst),
        "stub peer should have observed the request before closing"
    );
}

#[test]
fn ensure_daemon_fast_path_skips_stale_state_cleanup() {
    let tmp = tempfile::tempdir().unwrap();
    let (port, _saw_request) = spawn_silent_peer();
    write_daemon_info(tmp.path(), std::process::id(), port);
    write_unfinished_session(tmp.path(), "stale-session");

    ensure_daemon(tmp.path()).expect("reachable daemon should satisfy fast path");

    let session: SessionSnapshot =
        read_json_file(&session_snapshot_path(tmp.path(), "stale-session")).unwrap();
    assert_eq!(
        session.exit_code, None,
        "steady-state ensure_daemon must not scan and mutate session files"
    );
}

#[test]
fn send_daemon_request_defaults_to_prost_wire() {
    let _guard = EnvGuard::unset(super::super::wire_prost::ENV_DAEMON_WIRE);
    let tmp = tempfile::tempdir().unwrap();
    let (port, line_rx) = spawn_shutdown_ack_peer();
    write_daemon_info(tmp.path(), std::process::id(), port);

    let response = send_daemon_request(
        tmp.path(),
        &DaemonRequest::Shutdown {
            client_version: None,
            expected_daemon: None,
        },
    )
    .unwrap();
    assert!(matches!(
        response,
        DaemonResponse::ShutdownAck { pid: 4242 }
    ));
    let line = line_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(line.starts_with("CLUD-FRAME/1 434c5544 "));
}

#[test]
fn send_daemon_request_uses_legacy_json_wire_when_requested() {
    let _guard = EnvGuard::set(super::super::wire_prost::ENV_DAEMON_WIRE, "json");
    let tmp = tempfile::tempdir().unwrap();
    let (port, line_rx) = spawn_shutdown_ack_peer();
    write_daemon_info(tmp.path(), std::process::id(), port);

    let response = send_daemon_request(
        tmp.path(),
        &DaemonRequest::Shutdown {
            client_version: None,
            expected_daemon: None,
        },
    )
    .unwrap();
    assert!(matches!(
        response,
        DaemonResponse::ShutdownAck { pid: 4242 }
    ));
    let line = line_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(line.starts_with(r#"{"op":"shutdown"}"#));
}

#[test]
fn is_old_daemon_signature_recognizes_connection_drop_variants() {
    assert!(is_old_daemon_signature(&io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "x"
    )));
    assert!(is_old_daemon_signature(&io::Error::new(
        io::ErrorKind::ConnectionReset,
        "x"
    )));
    assert!(is_old_daemon_signature(&io::Error::new(
        io::ErrorKind::ConnectionAborted,
        "x"
    )));
    assert!(!is_old_daemon_signature(&io::Error::new(
        io::ErrorKind::NotFound,
        "x"
    )));
    assert!(!is_old_daemon_signature(&io::Error::new(
        io::ErrorKind::TimedOut,
        "x"
    )));
}

#[test]
fn request_daemon_shutdown_treats_dead_pid_as_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    write_daemon_info(tmp.path(), u32::MAX, 9);

    let err = request_daemon_shutdown(tmp.path())
        .expect_err("dead daemon pid should be treated as absent");
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
    assert!(
        !daemon_info_path(tmp.path()).exists(),
        "stale daemon.json should be removed"
    );
}

struct EnvGuard {
    key: &'static str,
    prior: Option<String>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn set(key: &'static str, value: &str) -> Self {
        let lock = Self::lock();
        let prior = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self {
            key,
            prior,
            _lock: lock,
        }
    }

    fn unset(key: &'static str) -> Self {
        let lock = Self::lock();
        let prior = std::env::var(key).ok();
        std::env::remove_var(key);
        Self {
            key,
            prior,
            _lock: lock,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.prior.take() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}
