use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use running_process::{CommandSpec, NativeProcess, ProcessConfig, StderrMode, StdinMode};
use sysinfo::Signal;

use crate::win_creation_flags::invisible_helper_creationflags;

use super::client::cleanup_stale_state;
use super::gc_service::{spawn_registry_worker_for_state, GcRequestMsg, WORKER_REPLY_TIMEOUT};
use super::http::{default_live_sessions_provider, spawn_dashboard};
use super::io_helpers::{new_session_id, read_json_file, write_json_file, write_json_line};
use super::paths::{daemon_info_path, session_snapshot_path, sessions_dir, spec_path, specs_dir};
use super::process_utils::{pid_is_alive, signal_process_tree};
use super::sessions::list_live_session_cwds;
use super::types::{
    DaemonInfo, DaemonRequest, DaemonResponse, GcReply, SessionSnapshot, WorkerLaunchSpec,
};

fn current_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(super) fn run_daemon(state_dir: &Path) -> i32 {
    if let Err(err) = fs::create_dir_all(state_dir) {
        eprintln!("[clud] failed to create daemon state dir: {}", err);
        return 1;
    }

    cleanup_stale_state(state_dir);

    let listener = match TcpListener::bind(("127.0.0.1", 0)) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("[clud] failed to bind daemon listener: {}", err);
            return 1;
        }
    };
    let port = match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(err) => {
            eprintln!("[clud] failed to read daemon listener addr: {}", err);
            return 1;
        }
    };
    // Issue #135: GC registry worker is in-process now. Failing to open
    // the registry is non-fatal — session ops still work; only GC ops
    // will error back to the CLI. Log once and continue.
    let gc_tx = match spawn_registry_worker_for_state(state_dir.to_path_buf()) {
        Ok(tx) => Some(tx),
        Err(err) => {
            eprintln!("[clud] note: gc registry unavailable: {}", err);
            None
        }
    };

    // Issue #183: in-process HTTP dashboard. Bind a second loopback port
    // alongside the IPC listener and run a `tiny_http` server on a worker
    // thread. The port is recorded in `daemon.json` so `clud ui` can
    // discover it. Bind failures are non-fatal — IPC keeps working;
    // `dashboard_port` is just `None` on this daemon instance.
    let started_at_unix = current_unix();
    let dashboard_port = spawn_dashboard(
        state_dir.to_path_buf(),
        gc_tx.clone(),
        port,
        started_at_unix,
        default_live_sessions_provider(),
    );

    let info = DaemonInfo {
        pid: std::process::id(),
        port,
        dashboard_port,
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
    };
    if let Err(err) = write_json_file(&daemon_info_path(state_dir), &info) {
        eprintln!("[clud] failed to persist daemon info: {}", err);
        return 1;
    }

    let workers = Arc::new(Mutex::new(HashMap::<String, Arc<NativeProcess>>::new()));
    let shutdown_requested = Arc::new(AtomicBool::new(false));
    if let Err(err) = listener.set_nonblocking(true) {
        eprintln!("[clud] failed to configure daemon listener: {}", err);
        return 1;
    }

    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                let workers = Arc::clone(&workers);
                let state_dir = state_dir.to_path_buf();
                let gc_tx = gc_tx.clone();
                let shutdown_requested = Arc::clone(&shutdown_requested);
                thread::spawn(move || {
                    let _ = handle_daemon_connection(
                        stream,
                        &state_dir,
                        &workers,
                        gc_tx,
                        &shutdown_requested,
                    );
                });
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                if shutdown_requested.load(Ordering::SeqCst) {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(err) => {
                if shutdown_requested.load(Ordering::SeqCst) {
                    break;
                }
                eprintln!("[clud] daemon listener accept failed: {}", err);
                thread::sleep(Duration::from_millis(50));
            }
        }
    }

    let _ = fs::remove_file(daemon_info_path(state_dir));
    0
}

fn handle_daemon_connection(
    mut stream: TcpStream,
    state_dir: &Path,
    workers: &Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>,
    gc_tx: Option<mpsc::Sender<GcRequestMsg>>,
    shutdown_requested: &Arc<AtomicBool>,
) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    let request: DaemonRequest = serde_json::from_str(&line)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    let response = match request {
        DaemonRequest::Create { spec } => match daemon_create_session(state_dir, workers, *spec) {
            Ok(session) => DaemonResponse::Created { session },
            Err(err) => DaemonResponse::Error {
                message: err.to_string(),
            },
        },
        DaemonRequest::Session { session_id } => {
            match read_json_file::<SessionSnapshot>(&session_snapshot_path(state_dir, &session_id))
            {
                Ok(session) => DaemonResponse::Session { session },
                Err(err) => DaemonResponse::Error {
                    message: err.to_string(),
                },
            }
        }
        DaemonRequest::ListLiveCwds => DaemonResponse::LiveCwds {
            paths: list_live_session_cwds(state_dir),
        },
        DaemonRequest::Terminate { session_id } => {
            match daemon_terminate_session(state_dir, workers, &session_id) {
                Ok(session) => DaemonResponse::Terminated { session },
                Err(err) => DaemonResponse::Error {
                    message: err.to_string(),
                },
            }
        }
        DaemonRequest::Gc { payload } => {
            let reply = dispatch_gc_op(gc_tx.as_ref(), payload);
            DaemonResponse::Gc { reply }
        }
        DaemonRequest::Shutdown => DaemonResponse::ShutdownAck {
            pid: std::process::id(),
        },
    };
    let is_shutdown = matches!(response, DaemonResponse::ShutdownAck { .. });
    let result = write_json_line(&mut stream, &response);
    if is_shutdown {
        let _ = stream.shutdown(std::net::Shutdown::Write);
        shutdown_requested.store(true, Ordering::SeqCst);
    }
    result
}

/// Hand a GC op to the registry worker and await the reply. Returns a
/// `GcReply::Error` if the worker is missing (failed to spawn at daemon
/// startup), hung up, or didn't reply within [`WORKER_REPLY_TIMEOUT`].
fn dispatch_gc_op(gc_tx: Option<&mpsc::Sender<GcRequestMsg>>, op: super::types::GcOp) -> GcReply {
    let Some(tx) = gc_tx else {
        return GcReply::Error {
            message: "gc registry unavailable in this daemon".to_string(),
        };
    };
    let (reply_tx, reply_rx) = mpsc::sync_channel::<GcReply>(1);
    if tx.send(GcRequestMsg { op, reply_tx }).is_err() {
        return GcReply::Error {
            message: "gc registry worker stopped".to_string(),
        };
    }
    reply_rx
        .recv_timeout(WORKER_REPLY_TIMEOUT)
        .unwrap_or_else(|_| GcReply::Error {
            message: "gc registry worker timed out".to_string(),
        })
}

fn daemon_create_session(
    state_dir: &Path,
    workers: &Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>,
    spec: WorkerLaunchSpec,
) -> io::Result<SessionSnapshot> {
    fs::create_dir_all(specs_dir(state_dir))?;
    fs::create_dir_all(sessions_dir(state_dir))?;

    let session_id = new_session_id();
    let spec_path = spec_path(state_dir, &session_id);
    write_json_file(&spec_path, &spec)?;

    let exe = std::env::current_exe()?;
    let worker = Arc::new(NativeProcess::new(ProcessConfig {
        command: CommandSpec::Argv(vec![
            exe.to_string_lossy().to_string(),
            "__worker".to_string(),
            "--state-dir".to_string(),
            state_dir.to_string_lossy().to_string(),
            "--session-id".to_string(),
            session_id.clone(),
            "--daemon-pid".to_string(),
            std::process::id().to_string(),
            "--spec-file".to_string(),
            spec_path.to_string_lossy().to_string(),
        ]),
        cwd: None,
        env: Some(std::env::vars().collect()),
        capture: false,
        stderr_mode: StderrMode::Stdout,
        // Issue #55: daemon-helper worker spawn — invisible by design.
        // stdio is `Null` and the user never sees this child's output
        // directly (output is forwarded via TCP to attaching clients),
        // so suppress the conhost window on Windows. No-op elsewhere.
        creationflags: invisible_helper_creationflags(),
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
        // `running-process-core` 3.4 removed the explicit `Containment`
        // knob; every `NativeProcess` is now automatically bound to a
        // kill-on-close Job Object on Windows. The worker can no longer
        // outlive the daemon on Windows, but the worker already polls
        // `pid_is_alive(daemon_pid)` to clean up if the daemon dies, so
        // the OS-level link only tightens that contract.
    }));
    worker
        .start()
        .map_err(|err| io::Error::other(err.to_string()))?;

    let started = Instant::now();
    let mut snapshot = loop {
        match read_json_file::<SessionSnapshot>(&session_snapshot_path(state_dir, &session_id)) {
            Ok(snapshot) => break snapshot,
            Err(err) if started.elapsed() < Duration::from_secs(5) => {
                let _ = err;
                thread::sleep(Duration::from_millis(25));
            }
            Err(err) => return Err(err),
        }
    };

    // Verify the worker's TCP listener is actually accepting connections
    // before reporting the session as ready. If the backend exits immediately,
    // the worker can persist the final snapshot and close its listener before
    // this readiness loop observes it; that is still a valid created session
    // whose failure can be inspected later.
    if snapshot.attachable {
        loop {
            if snapshot.exit_code.is_some() {
                break;
            }
            if TcpStream::connect(("127.0.0.1", snapshot.worker_port)).is_ok() {
                break;
            }
            if let Ok(updated) =
                read_json_file::<SessionSnapshot>(&session_snapshot_path(state_dir, &session_id))
            {
                snapshot = updated;
                if snapshot.exit_code.is_some() {
                    break;
                }
            }
            if started.elapsed() >= Duration::from_secs(5) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "worker wrote snapshot but TCP port {} is not accepting connections",
                        snapshot.worker_port
                    ),
                ));
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    workers
        .lock()
        .expect("workers mutex poisoned")
        .insert(session_id.clone(), Arc::clone(&worker));
    reap_worker_when_done(Arc::clone(workers), session_id.clone(), worker);
    Ok(snapshot)
}

fn reap_worker_when_done(
    workers: Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>,
    session_id: String,
    worker: Arc<NativeProcess>,
) {
    thread::spawn(move || {
        let _ = worker.wait(None);
        let mut guard = workers.lock().expect("workers mutex poisoned");
        if guard
            .get(&session_id)
            .is_some_and(|current| Arc::ptr_eq(current, &worker))
        {
            guard.remove(&session_id);
        }
    });
}

fn daemon_terminate_session(
    state_dir: &Path,
    workers: &Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>,
    session_id: &str,
) -> io::Result<SessionSnapshot> {
    let path = session_snapshot_path(state_dir, session_id);
    let mut session = read_json_file::<SessionSnapshot>(&path)?;

    if let Some(root_pid) = session.root_pid {
        signal_process_tree(root_pid, Signal::Term);
        thread::sleep(Duration::from_millis(150));
        signal_process_tree(root_pid, Signal::Kill);
    }

    if let Some(worker) = workers
        .lock()
        .expect("workers mutex poisoned")
        .remove(session_id)
    {
        let _ = worker.kill();
        let _ = worker.wait(Some(Duration::from_secs(2)));
    } else if pid_is_alive(session.worker_pid) {
        signal_process_tree(session.worker_pid, Signal::Term);
        thread::sleep(Duration::from_millis(150));
        signal_process_tree(session.worker_pid, Signal::Kill);
    }

    session.background = false;
    session.exit_code = Some(130);
    write_json_file(&path, &session)?;
    let _ = fs::remove_file(spec_path(state_dir, session_id));
    Ok(session)
}
