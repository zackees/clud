use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use running_process::{CommandSpec, NativeProcess, ProcessConfig, StderrMode, StdinMode};
use serde_json::json;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System};

use crate::orphan_reaper;
use crate::win_creation_flags::invisible_helper_creationflags;

use super::client::cleanup_stale_state;
use super::daemon_events;
use super::gc_service::{
    spawn_registry_worker_for_state, GcRequestMsg, RegistryMsg, WORKER_REPLY_TIMEOUT,
};
use super::http::{
    default_live_sessions_provider, spawn_dashboard_with_activity, TelemetryStore,
    ToolTelemetryStore,
};
use super::io_helpers::{new_session_id, read_json_file, write_json_file};
use super::paths::{
    daemon_events_path, daemon_info_path, session_snapshot_path, sessions_dir, spec_path, specs_dir,
};
use super::proc_sampler::{spawn_proc_sampler, ProcSamplerHandle, DEFAULT_SAMPLE_INTERVAL_MS};
use super::process_utils::{identity_is_alive, signal_process_tree_as};
use super::runtime_config::{DaemonRuntimeConfig, TestExpiryReason, TestRuntimeActivity};
use super::sessions::list_live_session_cwds;
use super::types::{
    unix_millis_now, CtrlCProfile, DaemonInfo, DaemonRequest, DaemonResponse, GcReply,
    SessionSnapshot, WorkerLaunchSpec,
};
use super::wire_prost::{
    decode_daemon_request_line, encode_daemon_response_line, DaemonWireFormat, WireError,
};

fn current_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn spawn_tool_installer() {
    let _ = thread::Builder::new()
        .name("clud-tool-install".to_string())
        .spawn(crate::tool_install::ensure_installed);
}

pub(super) fn run_daemon(state_dir: &Path) -> i32 {
    // Retag the crash reporter installed by main.rs so any crash inside the
    // daemon process gets written under role="daemon". `install_native`
    // also ensures the SIGSEGV / SIGBUS / SIGILL / SIGFPE / SIGABRT /
    // Windows-SEH handler is in place — both panic and native crashes go
    // to `~/.clud/state/crashes/`.
    crate::crash_report::install_native("daemon");
    let runtime_config = DaemonRuntimeConfig::from_env();
    let daemon_started_at = Instant::now();
    let test_activity = runtime_config
        .test_mode
        .then(|| TestRuntimeActivity::new(daemon_started_at));
    if let Err(err) = fs::create_dir_all(state_dir) {
        eprintln!("[clud] failed to create daemon state dir: {}", err);
        return 1;
    }
    daemon_events::log_event(state_dir, "daemon_starting", []);
    daemon_events::log_event(
        state_dir,
        "daemon_runtime_profile",
        [
            ("test_mode", json!(runtime_config.test_mode)),
            (
                "test_max_lifetime_secs",
                json!(runtime_config
                    .test_max_lifetime
                    .map(|value| value.as_secs())),
            ),
            (
                "test_idle_timeout_secs",
                json!(runtime_config
                    .test_idle_timeout
                    .map(|value| value.as_secs())),
            ),
            (
                "host_scans_enabled",
                json!(runtime_config.host_scans_enabled),
            ),
            (
                "periodic_maintenance_enabled",
                json!(runtime_config.periodic_maintenance_enabled),
            ),
        ],
    );

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
    let gc_tx = match spawn_registry_worker_for_state(
        state_dir.to_path_buf(),
        runtime_config.periodic_maintenance_enabled,
    ) {
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
    // Issue #469: in-memory telemetry sink for `clud log` POSTs. Lives
    // only for the daemon's lifetime — restart wipes it. Persistence
    // is a follow-up once the prototype contract stabilizes.
    let telemetry = TelemetryStore::new();
    let tool_telemetry = ToolTelemetryStore::new();
    let dashboard_port = spawn_dashboard_with_activity(
        state_dir.to_path_buf(),
        gc_tx.clone(),
        port,
        started_at_unix,
        default_live_sessions_provider(),
        telemetry,
        tool_telemetry,
        test_activity.clone(),
    );

    // Tool installation is deferred until after readiness. `clud tool` self-heals
    // its requested file inline, so daemon bringup no longer blocks callers
    // on a full bundled-tool scan.

    let info = DaemonInfo {
        pid: std::process::id(),
        pid_start: crate::process_identity::self_start_time(),
        port,
        dashboard_port,
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
    };
    if let Err(err) = write_json_file(&daemon_info_path(state_dir), &info) {
        eprintln!("[clud] failed to persist daemon info: {}", err);
        return 1;
    }
    daemon_events::log_event(
        state_dir,
        "daemon_started",
        [
            ("port", json!(port)),
            ("dashboard_port", json!(dashboard_port)),
            ("version", json!(env!("CARGO_PKG_VERSION"))),
            ("event_log", json!(daemon_events_path(state_dir))),
        ],
    );
    spawn_tool_installer();

    let workers = Arc::new(Mutex::new(HashMap::<String, Arc<NativeProcess>>::new()));
    let shutdown_requested = Arc::new(AtomicBool::new(false));
    let proc_sampler = if runtime_config.host_scans_enabled {
        daemon_events::log_event(state_dir, "proc_sampler_started", []);
        spawn_proc_sampler(state_dir.to_path_buf(), Arc::clone(&shutdown_requested))
    } else {
        ProcSamplerHandle::empty(DEFAULT_SAMPLE_INTERVAL_MS)
    };
    if let Err(err) = listener.set_nonblocking(true) {
        eprintln!("[clud] failed to configure daemon listener: {}", err);
        return 1;
    }

    // running-process consumer adoption (upstream #385): serve the broker
    // v1 frame lane (BackendHandle identity probes + clud payload frames
    // under protocol 0x7C4C) on a local-socket endpoint next to the TCP
    // listener. Best-effort: failure to bind never takes the daemon down,
    // and RUNNING_PROCESS_DISABLE=1 skips the lane entirely.
    let rp_lane = super::rp_broker::spawn_frame_lane(
        state_dir,
        Arc::clone(&workers),
        gc_tx.clone(),
        Arc::clone(&shutdown_requested),
        proc_sampler.clone(),
    );

    // Periodic orphan sweep. Catches CLUD-tagged descendants whose
    // originator clud was SIGKILL'd and never ran its own exit hook (so
    // the on-exit `ReapOrphans` IPC never fired). Pure background work —
    // never blocks the accept loop. Sleeps in 1s slices so shutdown is
    // promptly observed.
    if runtime_config.host_scans_enabled {
        daemon_events::log_event(state_dir, "orphan_sweeper_started", []);
        spawn_orphan_sweeper(state_dir.to_path_buf(), Arc::clone(&shutdown_requested));
    }

    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                let activity_guard = test_activity
                    .as_ref()
                    .map(TestRuntimeActivity::start_request);
                let workers = Arc::clone(&workers);
                let state_dir = state_dir.to_path_buf();
                let gc_tx = gc_tx.clone();
                let shutdown_requested = Arc::clone(&shutdown_requested);
                let proc_sampler = proc_sampler.clone();
                thread::spawn(move || {
                    let _activity_guard = activity_guard;
                    let _ = handle_daemon_connection(
                        stream,
                        &state_dir,
                        &workers,
                        gc_tx,
                        &shutdown_requested,
                        &proc_sampler,
                    );
                });
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                if shutdown_requested.load(Ordering::SeqCst) {
                    break;
                }
                let now = Instant::now();
                let worker_count = workers.lock().expect("workers mutex poisoned").len();
                if worker_count > 0 {
                    if let Some(activity) = &test_activity {
                        activity.note_activity(now);
                    }
                }
                let (idle_for, active_requests) = test_activity
                    .as_ref()
                    .map(|activity| activity.snapshot(now))
                    .unwrap_or((Duration::ZERO, 0));
                if let Some(reason) = runtime_config.test_expiry_reason(
                    now.duration_since(daemon_started_at),
                    idle_for,
                    worker_count,
                    active_requests,
                ) {
                    let (op, timeout) = match reason {
                        TestExpiryReason::Idle => {
                            ("daemon_test_idle_expired", runtime_config.test_idle_timeout)
                        }
                        TestExpiryReason::MaxLifetime => (
                            "daemon_test_max_lifetime_expired",
                            runtime_config.test_max_lifetime,
                        ),
                    };
                    daemon_events::log_event(
                        state_dir,
                        op,
                        [
                            ("timeout_secs", json!(timeout.map(|value| value.as_secs()))),
                            (
                                "uptime_ms",
                                json!(now.duration_since(daemon_started_at).as_millis()),
                            ),
                            ("idle_ms", json!(idle_for.as_millis())),
                            ("worker_count", json!(worker_count)),
                            ("active_requests", json!(active_requests)),
                        ],
                    );
                    shutdown_requested.store(true, Ordering::SeqCst);
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

    if let Some(lane) = rp_lane {
        lane.cleanup();
    }
    daemon_events::log_event(state_dir, "daemon_stopping", []);
    let _ = fs::remove_file(daemon_info_path(state_dir));
    daemon_events::log_event(state_dir, "daemon_stopped", []);
    0
}

fn handle_daemon_connection(
    mut stream: TcpStream,
    state_dir: &Path,
    workers: &Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>,
    gc_tx: Option<mpsc::Sender<RegistryMsg>>,
    shutdown_requested: &Arc<AtomicBool>,
    proc_sampler: &ProcSamplerHandle,
) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    let (request, response_format) = decode_daemon_request_line(&line).map_err(wire_error_to_io)?;
    let request_id = daemon_events::request_id();
    daemon_events::log_event(
        state_dir,
        "request_received",
        [
            ("request_id", json!(request_id)),
            ("request_op", json!(request_op(&request))),
            ("wire_format", json!(response_format_name(response_format))),
        ],
    );
    let started = Instant::now();
    let response = dispatch_daemon_request_with_id(
        state_dir,
        workers,
        gc_tx.as_ref(),
        Some(proc_sampler),
        request_id,
        request,
    );
    let is_shutdown = matches!(response, DaemonResponse::ShutdownAck { .. });
    let result = write_daemon_response(&mut stream, &response, response_format);
    daemon_events::log_event(
        state_dir,
        "request_replied",
        [
            ("request_id", json!(request_id)),
            ("response_op", json!(response_op(&response))),
            ("duration_ms", json!(started.elapsed().as_millis())),
            (
                "write_error",
                json!(result.as_ref().err().map(|err| err.to_string())),
            ),
        ],
    );
    if is_shutdown {
        let _ = stream.shutdown(std::net::Shutdown::Write);
        shutdown_requested.store(true, Ordering::SeqCst);
    }
    result
}

/// Map one decoded [`DaemonRequest`] to its [`DaemonResponse`].
///
/// Shared by both transport lanes — the legacy TCP line wire above and
/// the running-process broker v1 frame lane (`rp_broker`) — so request
/// semantics cannot drift between them. Transport concerns (encoding,
/// flagging shutdown after the reply is written) stay with each lane.
#[cfg(test)]
pub(super) fn dispatch_daemon_request(
    state_dir: &Path,
    workers: &Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>,
    gc_tx: Option<&mpsc::Sender<RegistryMsg>>,
    request: DaemonRequest,
) -> DaemonResponse {
    dispatch_daemon_request_with_sampler(state_dir, workers, gc_tx, None, request)
}

pub(super) fn dispatch_daemon_request_with_sampler(
    state_dir: &Path,
    workers: &Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>,
    gc_tx: Option<&mpsc::Sender<RegistryMsg>>,
    proc_sampler: Option<&ProcSamplerHandle>,
    request: DaemonRequest,
) -> DaemonResponse {
    let request_id = daemon_events::request_id();
    dispatch_daemon_request_with_id(state_dir, workers, gc_tx, proc_sampler, request_id, request)
}

fn dispatch_daemon_request_with_id(
    state_dir: &Path,
    workers: &Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>,
    gc_tx: Option<&mpsc::Sender<RegistryMsg>>,
    proc_sampler: Option<&ProcSamplerHandle>,
    request_id: u64,
    request: DaemonRequest,
) -> DaemonResponse {
    match request {
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
        DaemonRequest::Interrupt {
            session_id,
            profile,
        } => match daemon_interrupt_session(state_dir, workers, &session_id, profile) {
            Ok(session) => DaemonResponse::Interrupted { session },
            Err(err) => DaemonResponse::Error {
                message: err.to_string(),
            },
        },
        DaemonRequest::AdoptKill { pids, reason } => {
            daemon_events::log_event(
                state_dir,
                "adopt_kill_accepted",
                [
                    ("request_id", json!(request_id)),
                    ("pids", json!(pids)),
                    ("reason", json!(reason)),
                ],
            );
            spawn_adopt_kill_worker(state_dir.to_path_buf(), pids.clone(), reason);
            DaemonResponse::AdoptKillAck {
                accepted: pids.len(),
            }
        }
        DaemonRequest::ReapOrphans => {
            // Fire-and-forget: spawn the sweep on a background thread so the
            // CLI's exit path never blocks on `kill_tree`. Ack with zeros —
            // the foreground caller doesn't wait for the actual count.
            daemon_events::log_event(
                state_dir,
                "reap_orphans_accepted",
                [("request_id", json!(request_id))],
            );
            spawn_orphan_reap_once(state_dir.to_path_buf(), "request", Some(request_id));
            DaemonResponse::ReapOrphansAck {
                found: 0,
                reaped: 0,
            }
        }
        DaemonRequest::Metrics => DaemonResponse::Metrics {
            pid: std::process::id(),
            cpu_pct: sample_daemon_cpu_pct(),
        },
        DaemonRequest::ProcSnapshot {
            include_dead_since_ms,
        } => {
            let snapshot = proc_sampler
                .cloned()
                .unwrap_or_else(|| ProcSamplerHandle::empty(DEFAULT_SAMPLE_INTERVAL_MS))
                .snapshot(include_dead_since_ms);
            DaemonResponse::ProcSnapshot { snapshot }
        }
        DaemonRequest::Gc { payload } => {
            let reply = dispatch_gc_op(state_dir, gc_tx, request_id, payload);
            DaemonResponse::Gc { reply }
        }
        DaemonRequest::Shutdown => DaemonResponse::ShutdownAck {
            pid: std::process::id(),
        },
    }
}

fn write_daemon_response(
    stream: &mut TcpStream,
    response: &DaemonResponse,
    format: DaemonWireFormat,
) -> io::Result<()> {
    let bytes = encode_daemon_response_line(response, format).map_err(wire_error_to_io)?;
    stream.write_all(&bytes)?;
    stream.flush()
}

fn wire_error_to_io(err: WireError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err)
}

fn response_format_name(format: DaemonWireFormat) -> &'static str {
    match format {
        DaemonWireFormat::Json => "json",
        DaemonWireFormat::Prost => "prost",
    }
}

fn request_op(request: &DaemonRequest) -> &'static str {
    match request {
        DaemonRequest::Create { .. } => "create",
        DaemonRequest::Session { .. } => "session",
        DaemonRequest::ListLiveCwds => "list_live_cwds",
        DaemonRequest::Terminate { .. } => "terminate",
        DaemonRequest::Interrupt { .. } => "interrupt",
        DaemonRequest::AdoptKill { .. } => "adopt_kill",
        DaemonRequest::Gc { .. } => "gc",
        DaemonRequest::Shutdown => "shutdown",
        DaemonRequest::ReapOrphans => "reap_orphans",
        DaemonRequest::Metrics => "metrics",
        DaemonRequest::ProcSnapshot { .. } => "proc_snapshot",
    }
}

fn response_op(response: &DaemonResponse) -> &'static str {
    match response {
        DaemonResponse::Created { .. } => "created",
        DaemonResponse::Session { .. } => "session",
        DaemonResponse::LiveCwds { .. } => "live_cwds",
        DaemonResponse::Terminated { .. } => "terminated",
        DaemonResponse::Interrupted { .. } => "interrupted",
        DaemonResponse::AdoptKillAck { .. } => "adopt_kill_ack",
        DaemonResponse::Gc { .. } => "gc",
        DaemonResponse::ShutdownAck { .. } => "shutdown_ack",
        DaemonResponse::ReapOrphansAck { .. } => "reap_orphans_ack",
        DaemonResponse::Metrics { .. } => "metrics",
        DaemonResponse::ProcSnapshot { .. } => "proc_snapshot",
        DaemonResponse::Error { .. } => "error",
    }
}

fn sample_daemon_cpu_pct() -> f32 {
    static SAMPLER: std::sync::OnceLock<Mutex<System>> = std::sync::OnceLock::new();
    let mut sys = SAMPLER
        .get_or_init(|| Mutex::new(System::new()))
        .lock()
        .expect("daemon metrics sampler poisoned");
    let pid = Pid::from_u32(std::process::id());
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_cpu(),
    );
    sys.process(pid).map(|proc| proc.cpu_usage()).unwrap_or(0.0)
}

fn gc_reply_op(reply: &GcReply) -> &'static str {
    match reply {
        GcReply::WatchOk => "watch_ok",
        GcReply::ListOk { .. } => "list_ok",
        GcReply::PurgeOk { .. } => "purge_ok",
        GcReply::PurgeStarted { .. } => "purge_started",
        GcReply::ReconcileOk { .. } => "reconcile_ok",
        GcReply::InsertOk { .. } => "insert_ok",
        GcReply::RepoVisitOk => "repo_visit_ok",
        GcReply::RepoVisitsOk { .. } => "repo_visits_ok",
        GcReply::Error { .. } => "error",
    }
}

fn should_journal_reply(reply: &GcReply) -> bool {
    match reply {
        GcReply::WatchOk => false,
        GcReply::Error { .. } | GcReply::PurgeOk { .. } | GcReply::PurgeStarted { .. } => true,
        GcReply::ReconcileOk { inserted } => *inserted > 0,
        GcReply::InsertOk { inserted } => *inserted,
        GcReply::ListOk { .. } | GcReply::RepoVisitOk | GcReply::RepoVisitsOk { .. } => false,
    }
}

fn gc_trace_enabled() -> bool {
    std::env::var("CLUD_DAEMON_TRACE_GC").as_deref() == Ok("1")
}

/// Hand a GC op to the registry worker and await the reply. Returns a
/// `GcReply::Error` if the worker is missing (failed to spawn at daemon
/// startup), hung up, or didn't reply within [`WORKER_REPLY_TIMEOUT`].
fn dispatch_gc_op(
    state_dir: &Path,
    gc_tx: Option<&mpsc::Sender<RegistryMsg>>,
    request_id: u64,
    op: super::types::GcOp,
) -> GcReply {
    let trace = gc_trace_enabled();
    if trace {
        daemon_events::log_event(state_dir, "gc_started", [("request_id", json!(request_id))]);
    }
    let started = Instant::now();
    let Some(tx) = gc_tx else {
        let reply = GcReply::Error {
            message: "gc registry unavailable in this daemon".to_string(),
        };
        log_gc_finished(state_dir, request_id, started, &reply, trace);
        return reply;
    };
    let (reply_tx, reply_rx) = mpsc::sync_channel::<GcReply>(1);
    if tx
        .send(RegistryMsg::Op(GcRequestMsg { op, reply_tx }))
        .is_err()
    {
        let reply = GcReply::Error {
            message: "gc registry worker stopped".to_string(),
        };
        log_gc_finished(state_dir, request_id, started, &reply, trace);
        return reply;
    }
    let reply = reply_rx
        .recv_timeout(WORKER_REPLY_TIMEOUT)
        .unwrap_or_else(|_| GcReply::Error {
            message: "gc registry worker timed out".to_string(),
        });
    log_gc_finished(state_dir, request_id, started, &reply, trace);
    reply
}

fn log_gc_finished(
    state_dir: &Path,
    request_id: u64,
    started: Instant,
    reply: &GcReply,
    trace: bool,
) {
    if !trace && !should_journal_reply(reply) {
        return;
    }
    daemon_events::log_event(
        state_dir,
        "gc_finished",
        [
            ("request_id", json!(request_id)),
            ("response_op", json!(gc_reply_op(reply))),
            ("duration_ms", json!(started.elapsed().as_millis())),
        ],
    );
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

    // #465 handover registry: a daemon-launched detached/background session's
    // descendants are tagged with this daemon's PID as originator. Persist that
    // PID so a *successor* daemon (after a restart) spares the still-live
    // session instead of reaping it as a dead-originator orphan.
    if spec.detachable || spec.background_on_launch {
        super::handover_registry::register(state_dir, std::process::id());
    }

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
    reap_worker_when_done(
        state_dir.to_path_buf(),
        Arc::clone(workers),
        session_id.clone(),
        worker,
    );
    Ok(snapshot)
}

fn reap_worker_when_done(
    state_dir: std::path::PathBuf,
    workers: Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>,
    session_id: String,
    worker: Arc<NativeProcess>,
) {
    thread::spawn(move || {
        let started = Instant::now();
        daemon_events::log_event(
            &state_dir,
            "worker_reap_wait_started",
            [("session_id", json!(session_id))],
        );
        let wait_result = worker.wait(None);
        let mut guard = workers.lock().expect("workers mutex poisoned");
        let removed = if guard
            .get(&session_id)
            .is_some_and(|current| Arc::ptr_eq(current, &worker))
        {
            guard.remove(&session_id);
            true
        } else {
            false
        };
        daemon_events::log_event(
            &state_dir,
            "worker_reaped",
            [
                ("session_id", json!(session_id)),
                ("removed", json!(removed)),
                ("duration_ms", json!(started.elapsed().as_millis())),
                (
                    "wait_error",
                    json!(wait_result.err().map(|err| err.to_string())),
                ),
            ],
        );
    });
}

/// Background `kill_tree` for [`DaemonRequest::AdoptKill`].
///
/// The CLI hands us a list of root PIDs it was about to wait on and we
/// finish the job from out here so the foreground `clud` can drop the
/// user back to the shell immediately. Best-effort and fire-and-forget:
/// failures are silent because the parent CLI already returned 130 and
/// nobody is on the other end to receive a reply. The kill walk uses
/// `process_tree::kill_tree` for parity with the synchronous path it
/// replaces (see `runner::teardown_interrupted_child`).
/// Env override for the orphan-sweep period, in milliseconds (#465 AC 1).
const ENV_ORPHAN_SWEEP_INTERVAL_MS: &str = "CLUD_ORPHAN_SWEEP_INTERVAL_MS";
/// Default period between dead-originator orphan sweeps. 60s keeps the
/// full-host env scan (sysinfo + `environ` read for every process) off the
/// idle-CPU budget (#542) while still clearing SIGKILL'd-clud orphans within a
/// minute; the on-exit `Drop` guard and `clud slay` own the fast path.
const DEFAULT_ORPHAN_SWEEP_INTERVAL_MS: u64 = 60_000;
/// Floor so a misconfigured tiny value can't spin the sweep into a hot loop
/// (each sweep reads `environ` for every process on the host).
const MIN_ORPHAN_SWEEP_INTERVAL_MS: u64 = 5_000;

pub(super) fn orphan_sweep_interval() -> Duration {
    orphan_sweep_interval_from_raw(std::env::var(ENV_ORPHAN_SWEEP_INTERVAL_MS).ok().as_deref())
}

/// Resolve the sweep interval from a raw env value: parse, ignore
/// zero/garbage, clamp to the floor, else the 60s default. Pure so the
/// clamping and default are unit-tested without touching the environment.
fn orphan_sweep_interval_from_raw(raw: Option<&str>) -> Duration {
    let ms = raw
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|&ms| ms > 0)
        .unwrap_or(DEFAULT_ORPHAN_SWEEP_INTERVAL_MS)
        .max(MIN_ORPHAN_SWEEP_INTERVAL_MS);
    Duration::from_millis(ms)
}

fn spawn_orphan_sweeper(state_dir: std::path::PathBuf, shutdown_requested: Arc<AtomicBool>) {
    let interval = orphan_sweep_interval();
    let _ = thread::Builder::new()
        .name("clud-orphan-sweep".to_string())
        .spawn(move || {
            // When each still-present orphan candidate was first observed.
            // Thread-local to the sweeper: the grace window (#614) only
            // applies to this periodic path, never to an explicit request.
            let mut first_seen: HashMap<u32, Instant> = HashMap::new();
            loop {
                // Sleep in 1-second slices so shutdown is observed within ~1s.
                let mut remaining = interval;
                while remaining > Duration::ZERO {
                    if shutdown_requested.load(Ordering::SeqCst) {
                        return;
                    }
                    let slice = remaining.min(Duration::from_secs(1));
                    thread::sleep(slice);
                    remaining = remaining.saturating_sub(slice);
                }
                if shutdown_requested.load(Ordering::SeqCst) {
                    return;
                }
                run_orphan_sweep(&state_dir, "periodic", None, Some(&mut first_seen));
            }
        });
}

fn spawn_orphan_reap_once(
    state_dir: std::path::PathBuf,
    trigger: &'static str,
    request_id: Option<u64>,
) {
    thread::spawn(move || run_orphan_sweep(&state_dir, trigger, request_id, None));
}

/// Decide which candidates clear the grace window, and refresh `first_seen`.
///
/// Returns the admission closure's verdict for `pid`. A PID seen for the first
/// time is recorded and spared; on a later tick, once it has been visible for
/// [`ORPHAN_GRACE_MS`], it is admitted. Entries for PIDs that stop appearing
/// are pruned by the caller so the map cannot grow without bound.
fn grace_admits(first_seen: &mut HashMap<u32, Instant>, pid: u32, now: Instant) -> bool {
    let first = *first_seen.entry(pid).or_insert(now);
    now.duration_since(first) >= Duration::from_millis(orphan_reaper::ORPHAN_GRACE_MS)
}

/// Single-line record of the last completed orphan sweep, alongside the
/// existing `session-tmp-sweep.last` / `uv-cache-sweep.last` sentinels (#465).
pub(super) const ORPHAN_SWEEP_SENTINEL: &str = "orphan-sweep.last";

pub(super) fn orphan_sweep_sentinel_path(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join(ORPHAN_SWEEP_SENTINEL)
}

/// Best-effort: a sweep must never fail because its bookkeeping file could not
/// be written.
fn write_orphan_sweep_sentinel(state_dir: &Path, found: usize, reaped: usize) {
    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0);
    let line = json!({ "ts_ms": ts_ms, "found": found, "reaped": reaped }).to_string();
    let _ = std::fs::create_dir_all(state_dir);
    let _ = std::fs::write(orphan_sweep_sentinel_path(state_dir), line);
}

fn run_orphan_sweep(
    state_dir: &Path,
    trigger: &'static str,
    request_id: Option<u64>,
    grace: Option<&mut HashMap<u32, Instant>>,
) {
    daemon_events::log_event(
        state_dir,
        "orphan_sweep_started",
        [
            ("trigger", json!(trigger)),
            ("request_id", json!(request_id)),
        ],
    );
    let started = Instant::now();
    let opts = orphan_reaper::ReapOpts {
        keep: false,
        // Quiet: the daemon log shouldn't fill stderr with per-tick
        // scan reports; the JSONL stream is the durable diagnostic surface.
        quiet: true,
        explain: false,
    };
    // `deferred` is only meaningful on the periodic path; explicit requests
    // reap immediately (see `ORPHAN_GRACE_MS`).
    let mut deferred: Vec<u32> = Vec::new();
    let outcome = match grace {
        Some(first_seen) => {
            // #465 handover registry: spare intentionally-detached cohorts
            // (registered under the launching daemon's PID) so a successor
            // daemon doesn't reap a live detached session across a restart.
            // Applied only on the periodic path — an explicit `clud slay` /
            // `ReapOrphans` request means "reap now", registry or not.
            let spared = super::handover_registry::load(state_dir);
            let now = Instant::now();
            let mut observed: HashSet<u32> = HashSet::new();
            let (outcome, observed_origins) =
                orphan_reaper::reap_orphans_filtered_sparing(&opts, &spared, &mut |pid| {
                    observed.insert(pid);
                    let admitted = grace_admits(first_seen, pid, now);
                    if !admitted {
                        deferred.push(pid);
                    }
                    admitted
                });
            // Drop bookkeeping for PIDs that are gone, so a long-lived daemon
            // doesn't accumulate an entry per orphan it has ever seen.
            first_seen.retain(|pid, _| observed.contains(pid));
            // Drop handover entries whose originator no longer tags any live
            // process, bounding the registry's growth.
            super::handover_registry::prune(state_dir, &observed_origins);
            outcome
        }
        None => orphan_reaper::reap_orphans(&opts),
    };
    // Candidates that were selected but never handed to `kill_tree` — today
    // that is the `--keep-orphans` path. Previously hardcoded to `[]`, which
    // reported "nothing was skipped" even when something was (issue #612):
    // a kill log that cannot distinguish spared from reaped is not auditable.
    // Note this does NOT include PIDs pruned *inside* `kill_tree_filtered` by
    // the declared-daemon predicate; that walk is intentionally silent, so
    // those remain unobservable here.
    let skipped_pids: Vec<u32> = outcome
        .candidate_pids
        .iter()
        .filter(|pid| !outcome.reaped_pids.contains(pid))
        .copied()
        .collect();
    // #465: also record the result in a dedicated single-line sentinel, the
    // same pattern `session-tmp-sweep.last` / `uv-cache-sweep.last` already use
    // in this directory. `clud daemon orphan-status` reads this rather than
    // scanning the shared event log, which rotates at 1 MB — a burst of
    // unrelated high-volume events can otherwise push every sweep record out of
    // both log files and make a healthy daemon report "no sweep recorded".
    write_orphan_sweep_sentinel(state_dir, outcome.found, outcome.reaped);
    daemon_events::log_event(
        state_dir,
        "orphan_sweep_finished",
        [
            ("trigger", json!(trigger)),
            ("request_id", json!(request_id)),
            ("found", json!(outcome.found)),
            ("reaped", json!(outcome.reaped)),
            ("candidate_pids", json!(outcome.candidate_pids)),
            ("reaped_pids", json!(outcome.reaped_pids)),
            ("skipped_pids", json!(skipped_pids)),
            // Candidates held back by the grace window this pass; they become
            // eligible on a later tick if they are still around.
            ("deferred_pids", json!(deferred)),
            ("reason", json!("dead_originator")),
            ("duration_ms", json!(started.elapsed().as_millis())),
        ],
    );
}

fn spawn_adopt_kill_worker(state_dir: std::path::PathBuf, pids: Vec<u32>, reason: Option<String>) {
    let _ = thread::Builder::new()
        .name("clud-adopt-kill".to_string())
        .spawn(move || {
            daemon_events::log_event(
                &state_dir,
                "adopt_kill_started",
                [("pids", json!(pids)), ("reason", json!(reason))],
            );
            let started = Instant::now();
            let mut killed = Vec::new();
            for pid in &pids {
                let pid = *pid;
                crate::process_tree::kill_tree(pid);
                killed.push(pid);
            }
            daemon_events::log_event(
                &state_dir,
                "adopt_kill_finished",
                [
                    ("pids", json!(pids)),
                    ("killed", json!(killed)),
                    ("reason", json!(reason)),
                    ("duration_ms", json!(started.elapsed().as_millis())),
                ],
            );
        });
}

/// Tear down the processes a session owns: the agent tree first, then the
/// worker that supervises it.
///
/// Every PID here comes off disk, so each is re-checked against the start
/// time recorded next to it (issue #558) before anything is signalled. On a
/// busy Windows host the worker of a long-finished session can easily have
/// had its PID handed to something unrelated, and `clud kill` would otherwise
/// take that process down instead. A live `NativeProcess` handle, when the
/// daemon still owns one, is immune to the problem and is preferred.
fn terminate_session_processes(
    session: &SessionSnapshot,
    workers: &Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>,
    session_id: &str,
) {
    if let Some(root) = session.root_identity() {
        signal_process_tree_as(&root, Signal::Term);
        thread::sleep(Duration::from_millis(150));
        signal_process_tree_as(&root, Signal::Kill);
    }

    if let Some(worker) = workers
        .lock()
        .expect("workers mutex poisoned")
        .remove(session_id)
    {
        let _ = worker.kill();
        let _ = worker.wait(Some(Duration::from_secs(2)));
    } else {
        let worker = session.worker_identity();
        if identity_is_alive(&worker) {
            signal_process_tree_as(&worker, Signal::Term);
            thread::sleep(Duration::from_millis(150));
            signal_process_tree_as(&worker, Signal::Kill);
        }
    }
}

fn daemon_terminate_session(
    state_dir: &Path,
    workers: &Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>,
    session_id: &str,
) -> io::Result<SessionSnapshot> {
    let path = session_snapshot_path(state_dir, session_id);
    let mut session = read_json_file::<SessionSnapshot>(&path)?;

    terminate_session_processes(&session, workers, session_id);

    session.background = false;
    session.exit_code = Some(130);
    write_json_file(&path, &session)?;
    let _ = fs::remove_file(spec_path(state_dir, session_id));
    Ok(session)
}

fn daemon_interrupt_session(
    state_dir: &Path,
    workers: &Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>,
    session_id: &str,
    profile: CtrlCProfile,
) -> io::Result<SessionSnapshot> {
    let path = session_snapshot_path(state_dir, session_id);
    let mut session = read_json_file::<SessionSnapshot>(&path)?;
    merge_ctrl_c_profile_for_daemon(&mut session, profile);
    session.background = false;
    session.exit_code = Some(130);
    write_json_file(&path, &session)?;

    let state_dir = state_dir.to_path_buf();
    let workers = Arc::clone(workers);
    let session_id = session_id.to_string();
    thread::spawn(move || {
        finish_daemon_interrupt_session(&state_dir, &workers, &session_id);
    });

    Ok(session)
}

fn finish_daemon_interrupt_session(
    state_dir: &Path,
    workers: &Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>,
    session_id: &str,
) {
    let path = session_snapshot_path(state_dir, session_id);
    let mut session = match read_json_file::<SessionSnapshot>(&path) {
        Ok(session) => session,
        Err(_) => return,
    };
    let started_at_ms = unix_millis_now();
    {
        let profile = session.ctrl_c.get_or_insert_with(CtrlCProfile::default);
        if profile.daemon_kill_started_at_ms.is_none() {
            profile.daemon_kill_started_at_ms = Some(started_at_ms);
        }
        profile.fast_path = true;
    }
    let _ = write_json_file(&path, &session);

    terminate_session_processes(&session, workers, session_id);

    let finished_at_ms = unix_millis_now();
    if let Ok(mut latest) = read_json_file::<SessionSnapshot>(&path) {
        latest.background = false;
        latest.exit_code = Some(130);
        let profile = latest.ctrl_c.get_or_insert_with(CtrlCProfile::default);
        if profile.daemon_kill_started_at_ms.is_none() {
            profile.daemon_kill_started_at_ms = Some(started_at_ms);
        }
        profile.daemon_kill_finished_at_ms = Some(finished_at_ms);
        profile.daemon_kill_ms = Some(finished_at_ms.saturating_sub(started_at_ms));
        profile.fast_path = true;
        let _ = write_json_file(&path, &latest);
    }
    let _ = fs::remove_file(spec_path(state_dir, session_id));
}

fn merge_ctrl_c_profile_for_daemon(session: &mut SessionSnapshot, mut update: CtrlCProfile) {
    let now = unix_millis_now();
    update.fast_path = true;
    if update.daemon_received_at_ms.is_none() {
        update.daemon_received_at_ms = Some(now);
    }
    let current = session.ctrl_c.get_or_insert_with(CtrlCProfile::default);
    if update.cli_pid.is_some() {
        current.cli_pid = update.cli_pid;
    }
    if update.cli_observed_at_ms.is_some() {
        current.cli_observed_at_ms = update.cli_observed_at_ms;
    }
    if update.cli_handoff_at_ms.is_some() {
        current.cli_handoff_at_ms = update.cli_handoff_at_ms;
    }
    if update.cli_return_ready_at_ms.is_some() {
        current.cli_return_ready_at_ms = update.cli_return_ready_at_ms;
    }
    if update.cli_handoff_ms.is_some() {
        current.cli_handoff_ms = update.cli_handoff_ms;
    }
    if update.daemon_received_at_ms.is_some() {
        current.daemon_received_at_ms = update.daemon_received_at_ms;
    }
    current.fast_path = true;
}

#[cfg(test)]
mod tests {
    use super::super::wire_prost::{
        decode_daemon_response_line, encode_daemon_request_line, DaemonWireFormat,
    };
    use super::*;
    use std::io::{BufRead, Write};
    use std::net::TcpStream;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn orphan_sweep_interval_defaults_and_clamps() {
        // Unset / garbage / zero → the 60s default (#465 AC 1).
        assert_eq!(
            orphan_sweep_interval_from_raw(None),
            Duration::from_millis(DEFAULT_ORPHAN_SWEEP_INTERVAL_MS)
        );
        assert_eq!(
            orphan_sweep_interval_from_raw(Some("not-a-number")),
            Duration::from_millis(DEFAULT_ORPHAN_SWEEP_INTERVAL_MS)
        );
        assert_eq!(
            orphan_sweep_interval_from_raw(Some("0")),
            Duration::from_millis(DEFAULT_ORPHAN_SWEEP_INTERVAL_MS)
        );
        // A valid override is honored (with surrounding whitespace trimmed).
        assert_eq!(
            orphan_sweep_interval_from_raw(Some("  120000 ")),
            Duration::from_millis(120_000)
        );
        // A too-small value is clamped to the floor, never a hot loop.
        assert_eq!(
            orphan_sweep_interval_from_raw(Some("10")),
            Duration::from_millis(MIN_ORPHAN_SWEEP_INTERVAL_MS)
        );
    }

    // #380 + #387: even after the N=2->8 / N=9->25 sample bump, 1.2× was
    // still below the median's noise floor on macOS x86 (σ ≈ 40-50ms over
    // a ~50-195ms range → SE of median ~10ms, so the 95% CIs of JSON and
    // prost medians overlapped at the old 1.2× threshold). 1.5× still
    // catches a real >50% prost regression while staying above the noise.
    // Tightening below this would require either >=100 samples or a
    // trimmed-mean statistic — both deferred to a future perf-test
    // redesign.
    const PROST_PERF_BUDGET_NUMERATOR: u128 = 150;
    const PROST_PERF_BUDGET_DENOMINATOR: u128 = 100;
    // #380: macOS ARM runners exhibit bimodal latency (fast cluster ~50ms,
    // slow cluster ~200ms). At N=9 the JSON and prost medians can land on
    // opposite sides of the cluster gap purely by sample-count luck,
    // flagging a false-positive budget violation. Bump warmup to settle the
    // daemon process JIT / OS scheduler, and bump measured samples so the
    // median's σ/√N variance drops below the 1.5× budget margin.
    const DAEMON_WIRE_PERF_WARMUP_SAMPLES: usize = 8;
    const DAEMON_WIRE_PERF_MEASURED_SAMPLES: usize = 25;

    fn gc_event_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("gc event test lock poisoned")
    }

    fn test_gc_insert(path: &str) -> DaemonRequest {
        DaemonRequest::Gc {
            payload: super::super::types::GcOp::Insert {
                kind: "worktree".to_string(),
                path: path.to_string(),
                repo_root: None,
                branch: None,
                agent_id: None,
                created_unix: Some(100),
            },
        }
    }

    fn test_gc_worker(tmp: &Path) -> mpsc::Sender<RegistryMsg> {
        let registry = crate::gc::Registry::open_at(&tmp.join("gc.redb")).unwrap();
        super::super::gc_service::spawn_registry_worker_with(registry).unwrap()
    }

    #[test]
    fn should_journal_reply_covers_variant_matrix() {
        let rows = Vec::new();
        let cases = [
            (GcReply::ListOk { rows: rows.clone() }, false),
            (
                GcReply::PurgeOk {
                    removed: 0,
                    skipped: 0,
                },
                true,
            ),
            (
                GcReply::PurgeStarted {
                    dispatched: 0,
                    skipped: 0,
                },
                true,
            ),
            (GcReply::ReconcileOk { inserted: 0 }, false),
            (GcReply::ReconcileOk { inserted: 1 }, true),
            (GcReply::InsertOk { inserted: false }, false),
            (GcReply::InsertOk { inserted: true }, true),
            (GcReply::RepoVisitOk, false),
            (GcReply::RepoVisitsOk { rows: Vec::new() }, false),
            (
                GcReply::Error {
                    message: "failed".to_string(),
                },
                true,
            ),
        ];
        for (reply, expected) in cases {
            assert_eq!(should_journal_reply(&reply), expected, "{reply:?}");
        }
    }

    #[test]
    fn noop_insert_dispatch_writes_no_events() {
        let _guard = gc_event_test_lock();
        let tmp = tempfile::tempdir().unwrap();
        let workers = Arc::new(Mutex::new(HashMap::<String, Arc<NativeProcess>>::new()));
        let tx = test_gc_worker(tmp.path());

        let first = dispatch_daemon_request(
            tmp.path(),
            &workers,
            Some(&tx),
            test_gc_insert("/tmp/journal-noop"),
        );
        assert!(matches!(
            first,
            DaemonResponse::Gc {
                reply: GcReply::InsertOk { inserted: true }
            }
        ));
        let after_first = read_daemon_events(tmp.path());
        assert_eq!(after_first.len(), 1);
        assert_eq!(after_first[0]["op"], "gc_finished");

        let second = dispatch_daemon_request(
            tmp.path(),
            &workers,
            Some(&tx),
            test_gc_insert("/tmp/journal-noop"),
        );
        assert!(matches!(
            second,
            DaemonResponse::Gc {
                reply: GcReply::InsertOk { inserted: false }
            }
        ));
        assert_eq!(read_daemon_events(tmp.path()).len(), after_first.len());
    }

    #[test]
    fn mutating_and_error_ops_still_journal_gc_finished() {
        let _guard = gc_event_test_lock();
        let tmp = tempfile::tempdir().unwrap();
        let workers = Arc::new(Mutex::new(HashMap::<String, Arc<NativeProcess>>::new()));
        let tx = test_gc_worker(tmp.path());
        let _ = dispatch_daemon_request(
            tmp.path(),
            &workers,
            Some(&tx),
            test_gc_insert("/tmp/journal-mutation"),
        );
        let _ = dispatch_daemon_request(
            tmp.path(),
            &workers,
            None,
            test_gc_insert("/tmp/journal-error"),
        );
        let events = read_daemon_events(tmp.path());
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|event| event["op"] == "gc_finished"));
        assert_eq!(events[0]["response_op"], "insert_ok");
        assert_eq!(events[1]["response_op"], "error");
    }

    #[test]
    fn trace_gate_restores_full_stream() {
        let _guard = gc_event_test_lock();
        let prior = std::env::var_os("CLUD_DAEMON_TRACE_GC");
        std::env::set_var("CLUD_DAEMON_TRACE_GC", "1");
        struct Restore(Option<std::ffi::OsString>);
        impl Drop for Restore {
            fn drop(&mut self) {
                if let Some(value) = self.0.take() {
                    std::env::set_var("CLUD_DAEMON_TRACE_GC", value);
                } else {
                    std::env::remove_var("CLUD_DAEMON_TRACE_GC");
                }
            }
        }
        let _restore = Restore(prior);
        let tmp = tempfile::tempdir().unwrap();
        let workers = Arc::new(Mutex::new(HashMap::<String, Arc<NativeProcess>>::new()));
        let tx = test_gc_worker(tmp.path());
        let _ = dispatch_daemon_request(
            tmp.path(),
            &workers,
            Some(&tx),
            test_gc_insert("/tmp/journal-trace"),
        );
        let _ = dispatch_daemon_request(
            tmp.path(),
            &workers,
            Some(&tx),
            test_gc_insert("/tmp/journal-trace"),
        );
        let events = read_daemon_events(tmp.path());
        let ops: Vec<&str> = events
            .iter()
            .map(|event| event["op"].as_str().unwrap())
            .collect();
        assert_eq!(
            ops,
            ["gc_started", "gc_finished", "gc_started", "gc_finished"]
        );
    }

    fn read_daemon_events(state_dir: &Path) -> Vec<serde_json::Value> {
        let path = daemon_events_path(state_dir);
        let Ok(text) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        text.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn wait_for_daemon_event_ops(state_dir: &Path, expected: &[&str]) -> Vec<serde_json::Value> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let events = read_daemon_events(state_dir);
            let ops: Vec<&str> = events
                .iter()
                .filter_map(|event| event["op"].as_str())
                .collect();
            if expected.iter().all(|expected| ops.contains(expected)) {
                return events;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for daemon events {expected:?}; saw {ops:?}"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn wait_for_daemon_ready(state_dir: &Path) -> DaemonInfo {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(info) = read_json_file::<DaemonInfo>(&daemon_info_path(state_dir)) {
                if TcpStream::connect(("127.0.0.1", info.port)).is_ok() {
                    return info;
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for daemon startup"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn send_daemon_request_line(
        state_dir: &Path,
        request: &DaemonRequest,
        format: DaemonWireFormat,
    ) -> (DaemonResponse, String) {
        let info = read_json_file::<DaemonInfo>(&daemon_info_path(state_dir)).unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", info.port)).unwrap();
        let bytes = encode_daemon_request_line(request, format).unwrap();
        stream.write_all(&bytes).unwrap();
        stream.flush().unwrap();

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let response = decode_daemon_response_line(&line).unwrap();
        (response, line)
    }

    fn timed_daemon_request_line(
        state_dir: &Path,
        request: &DaemonRequest,
        format: DaemonWireFormat,
    ) -> (DaemonResponse, String, Duration) {
        let started = Instant::now();
        let (response, line) = send_daemon_request_line(state_dir, request, format);
        (response, line, started.elapsed())
    }

    fn median_duration(samples: &mut [Duration]) -> Duration {
        samples.sort_unstable();
        samples[samples.len() / 2]
    }

    fn scaled_duration(sample: Duration, numerator: u128, denominator: u128) -> Duration {
        let nanos = sample.as_nanos().saturating_mul(numerator) / denominator;
        Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64)
    }

    fn send_raw_daemon_request_line(state_dir: &Path, request_line: &str) -> String {
        let info = read_json_file::<DaemonInfo>(&daemon_info_path(state_dir)).unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", info.port)).unwrap();
        stream.write_all(request_line.as_bytes()).unwrap();
        stream.write_all(b"\n").unwrap();
        stream.flush().unwrap();

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        line
    }

    #[test]
    fn daemon_accepts_legacy_json_and_prost_clients_in_one_process() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().to_path_buf();
        let daemon_state_dir = state_dir.clone();
        let daemon_thread = thread::spawn(move || run_daemon(&daemon_state_dir));

        let info = wait_for_daemon_ready(&state_dir);
        let (json_response, json_line) = send_daemon_request_line(
            &state_dir,
            &DaemonRequest::ListLiveCwds,
            DaemonWireFormat::Json,
        );
        assert!(matches!(json_response, DaemonResponse::LiveCwds { .. }));
        assert!(
            json_line.starts_with(r#"{"op":"live_cwds""#),
            "default legacy request should receive a JSON daemon response: {json_line:?}"
        );

        let after_json = read_json_file::<DaemonInfo>(&daemon_info_path(&state_dir)).unwrap();
        assert_eq!(after_json.pid, info.pid);
        assert_eq!(after_json.port, info.port);

        let (prost_response, prost_line) = send_daemon_request_line(
            &state_dir,
            &DaemonRequest::Shutdown,
            DaemonWireFormat::Prost,
        );
        assert!(matches!(
            prost_response,
            DaemonResponse::ShutdownAck { pid } if pid == std::process::id()
        ));
        assert!(
            prost_line.starts_with("CLUD-FRAME/1 434c5544 "),
            "prost request should receive a prost daemon response: {prost_line:?}"
        );
        assert_eq!(daemon_thread.join().unwrap(), 0);
        assert!(
            !daemon_info_path(&state_dir).exists(),
            "daemon should remove daemon.json during shutdown"
        );
    }

    /// `spawn_adopt_kill_worker` must return immediately — the whole
    /// point of the AdoptKill IPC is that the CLI's wait time is bounded
    /// by an `mpsc` thread-spawn, not by the eventual `kill_tree` walk.
    /// Hand it a PID that's almost certainly dead so `kill_tree` returns
    /// fast, and pin the spawn-side latency below 100ms even on slow CI.
    #[test]
    fn spawn_adopt_kill_worker_returns_promptly() {
        let tmp = tempfile::tempdir().unwrap();
        let started = Instant::now();
        spawn_adopt_kill_worker(
            tmp.path().to_path_buf(),
            vec![u32::MAX],
            Some("test".to_string()),
        );
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "spawn took too long: {:?}",
            started.elapsed()
        );
    }

    /// Previous-release clients defaulted to JSON lines and did not know
    /// about the current prost encoder. Keep this fixture raw so it cannot
    /// accidentally track current client helper behavior.
    #[test]
    fn daemon_accepts_previous_release_raw_json_client_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().to_path_buf();
        let daemon_state_dir = state_dir.clone();
        let daemon_thread = thread::spawn(move || run_daemon(&daemon_state_dir));

        let info = wait_for_daemon_ready(&state_dir);
        let list_line = send_raw_daemon_request_line(&state_dir, r#"{"op":"list_live_cwds"}"#);
        assert!(
            !list_line.starts_with("CLUD-FRAME/1 "),
            "raw legacy JSON clients must receive legacy JSON replies: {list_line:?}"
        );
        let list_json: serde_json::Value = serde_json::from_str(&list_line).unwrap();
        assert_eq!(list_json["op"], "live_cwds");
        assert!(
            list_json["paths"].is_array(),
            "live_cwds JSON response should keep the previous-release paths array: {list_line:?}"
        );

        let shutdown_line = send_raw_daemon_request_line(&state_dir, r#"{"op":"shutdown"}"#);
        assert!(
            !shutdown_line.starts_with("CLUD-FRAME/1 "),
            "raw legacy JSON shutdown must receive a legacy JSON reply: {shutdown_line:?}"
        );
        let shutdown_json: serde_json::Value = serde_json::from_str(&shutdown_line).unwrap();
        assert_eq!(shutdown_json["op"], "shutdown_ack");
        assert_eq!(
            shutdown_json["pid"].as_u64(),
            Some(u64::from(info.pid)),
            "shutdown ack should identify the running daemon pid"
        );
        assert_eq!(daemon_thread.join().unwrap(), 0);
        assert!(
            !daemon_info_path(&state_dir).exists(),
            "daemon should remove daemon.json during raw JSON shutdown"
        );
    }

    #[test]
    fn prost_daemon_wire_list_rpc_stays_within_json_latency_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().to_path_buf();
        let daemon_state_dir = state_dir.clone();
        let daemon_thread = thread::spawn(move || run_daemon(&daemon_state_dir));

        wait_for_daemon_ready(&state_dir);
        for _ in 0..DAEMON_WIRE_PERF_WARMUP_SAMPLES {
            let (json_response, _) = send_daemon_request_line(
                &state_dir,
                &DaemonRequest::ListLiveCwds,
                DaemonWireFormat::Json,
            );
            assert!(matches!(json_response, DaemonResponse::LiveCwds { .. }));
            let (prost_response, _) = send_daemon_request_line(
                &state_dir,
                &DaemonRequest::ListLiveCwds,
                DaemonWireFormat::Prost,
            );
            assert!(matches!(prost_response, DaemonResponse::LiveCwds { .. }));
        }

        let mut json_samples = Vec::with_capacity(DAEMON_WIRE_PERF_MEASURED_SAMPLES);
        let mut prost_samples = Vec::with_capacity(DAEMON_WIRE_PERF_MEASURED_SAMPLES);
        for sample in 0..DAEMON_WIRE_PERF_MEASURED_SAMPLES {
            if sample % 2 == 0 {
                let (json_response, _, json_elapsed) = timed_daemon_request_line(
                    &state_dir,
                    &DaemonRequest::ListLiveCwds,
                    DaemonWireFormat::Json,
                );
                assert!(matches!(json_response, DaemonResponse::LiveCwds { .. }));
                json_samples.push(json_elapsed);

                let (prost_response, _, prost_elapsed) = timed_daemon_request_line(
                    &state_dir,
                    &DaemonRequest::ListLiveCwds,
                    DaemonWireFormat::Prost,
                );
                assert!(matches!(prost_response, DaemonResponse::LiveCwds { .. }));
                prost_samples.push(prost_elapsed);
            } else {
                let (prost_response, _, prost_elapsed) = timed_daemon_request_line(
                    &state_dir,
                    &DaemonRequest::ListLiveCwds,
                    DaemonWireFormat::Prost,
                );
                assert!(matches!(prost_response, DaemonResponse::LiveCwds { .. }));
                prost_samples.push(prost_elapsed);

                let (json_response, _, json_elapsed) = timed_daemon_request_line(
                    &state_dir,
                    &DaemonRequest::ListLiveCwds,
                    DaemonWireFormat::Json,
                );
                assert!(matches!(json_response, DaemonResponse::LiveCwds { .. }));
                json_samples.push(json_elapsed);
            }
        }

        let json_median = median_duration(&mut json_samples);
        let prost_median = median_duration(&mut prost_samples);
        let budget = scaled_duration(
            json_median,
            PROST_PERF_BUDGET_NUMERATOR,
            PROST_PERF_BUDGET_DENOMINATOR,
        );
        assert!(
            prost_median <= budget,
            "prost ListLiveCwds median latency {prost_median:?} exceeded 50% JSON budget {budget:?}; JSON median {json_median:?}; JSON samples {json_samples:?}; prost samples {prost_samples:?}"
        );

        let (shutdown_response, shutdown_line) = send_daemon_request_line(
            &state_dir,
            &DaemonRequest::Shutdown,
            DaemonWireFormat::Prost,
        );
        assert!(matches!(
            shutdown_response,
            DaemonResponse::ShutdownAck { pid } if pid == std::process::id()
        ));
        assert!(
            shutdown_line.starts_with("CLUD-FRAME/1 434c5544 "),
            "prost shutdown should receive a prost daemon response: {shutdown_line:?}"
        );
        assert_eq!(daemon_thread.join().unwrap(), 0);
    }

    #[test]
    fn spawn_adopt_kill_worker_accepts_empty_pids() {
        // Zero-PID payload is valid wire data (the CLI may have lost the
        // root PID); the worker must still spawn without panicking.
        let tmp = tempfile::tempdir().unwrap();
        let started = Instant::now();
        spawn_adopt_kill_worker(tmp.path().to_path_buf(), Vec::new(), None);
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn adopt_kill_request_writes_jsonl_events() {
        let tmp = tempfile::tempdir().unwrap();
        let workers = Arc::new(Mutex::new(HashMap::<String, Arc<NativeProcess>>::new()));

        let response = dispatch_daemon_request(
            tmp.path(),
            &workers,
            None,
            DaemonRequest::AdoptKill {
                pids: vec![u32::MAX],
                reason: Some("ctrl_c_subprocess".to_string()),
            },
        );
        assert!(matches!(
            response,
            DaemonResponse::AdoptKillAck { accepted: 1 }
        ));

        let events = wait_for_daemon_event_ops(
            tmp.path(),
            &[
                "adopt_kill_accepted",
                "adopt_kill_started",
                "adopt_kill_finished",
            ],
        );
        let accepted = events
            .iter()
            .find(|event| event["op"] == "adopt_kill_accepted")
            .unwrap();
        assert_eq!(accepted["pids"], json!([u32::MAX]));
        assert_eq!(accepted["reason"], "ctrl_c_subprocess");
        let finished = events
            .iter()
            .find(|event| event["op"] == "adopt_kill_finished")
            .unwrap();
        assert_eq!(finished["killed"], json!([u32::MAX]));
        assert!(finished["duration_ms"].is_u64());
    }

    #[test]
    fn reap_orphans_request_writes_jsonl_events() {
        let tmp = tempfile::tempdir().unwrap();
        let workers = Arc::new(Mutex::new(HashMap::<String, Arc<NativeProcess>>::new()));

        let response =
            dispatch_daemon_request(tmp.path(), &workers, None, DaemonRequest::ReapOrphans);
        assert!(matches!(
            response,
            DaemonResponse::ReapOrphansAck {
                found: 0,
                reaped: 0
            }
        ));

        let events = wait_for_daemon_event_ops(
            tmp.path(),
            &[
                "reap_orphans_accepted",
                "orphan_sweep_started",
                "orphan_sweep_finished",
            ],
        );
        let finished = events
            .iter()
            .find(|event| event["op"] == "orphan_sweep_finished")
            .unwrap();
        assert_eq!(finished["trigger"], "request");
        assert!(finished["found"].is_u64());
        assert!(finished["reaped"].is_u64());
        assert!(finished["candidate_pids"].is_array());
        assert!(finished["reaped_pids"].is_array());
        assert_eq!(finished["reason"], "dead_originator");
    }

    #[test]
    fn metrics_request_returns_daemon_pid_and_cpu_sample() {
        let tmp = tempfile::tempdir().unwrap();
        let workers = Arc::new(Mutex::new(HashMap::<String, Arc<NativeProcess>>::new()));

        let response = dispatch_daemon_request(tmp.path(), &workers, None, DaemonRequest::Metrics);
        match response {
            DaemonResponse::Metrics { pid, cpu_pct } => {
                assert_eq!(pid, std::process::id());
                assert!(cpu_pct >= 0.0, "cpu_pct should be non-negative: {cpu_pct}");
            }
            other => panic!("expected Metrics, got {other:?}"),
        }
    }

    #[test]
    fn proc_snapshot_request_returns_cached_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let workers = Arc::new(Mutex::new(HashMap::<String, Arc<NativeProcess>>::new()));

        let response = dispatch_daemon_request(
            tmp.path(),
            &workers,
            None,
            DaemonRequest::ProcSnapshot {
                include_dead_since_ms: 0,
            },
        );
        match response {
            DaemonResponse::ProcSnapshot { snapshot } => {
                assert_eq!(snapshot.schema_version, 1);
                assert_eq!(snapshot.sampler_pid, std::process::id());
            }
            other => panic!("expected ProcSnapshot, got {other:?}"),
        }
    }

    /// Issue #614: the periodic sweep must not reap a candidate the first
    /// time it sees it. The on-exit `Drop` guard of the clud that just died
    /// may still be working on that subtree.
    #[test]
    fn grace_window_spares_a_newly_observed_orphan() {
        let mut first_seen: HashMap<u32, Instant> = HashMap::new();
        let now = Instant::now();
        assert!(!grace_admits(&mut first_seen, 4321, now));
        assert!(first_seen.contains_key(&4321), "first sighting is recorded");
    }

    /// Once the candidate has been visible for the whole window it is
    /// admitted. Uses a synthetic "now" in the future rather than sleeping.
    #[test]
    fn grace_window_admits_after_the_window_elapses() {
        let mut first_seen: HashMap<u32, Instant> = HashMap::new();
        let now = Instant::now();
        assert!(!grace_admits(&mut first_seen, 4321, now));
        let later = now + Duration::from_millis(orphan_reaper::ORPHAN_GRACE_MS);
        assert!(grace_admits(&mut first_seen, 4321, later));
    }

    /// A candidate still inside the window on a later tick stays spared --
    /// the clock runs from first sighting, not from the current tick.
    #[test]
    fn grace_window_still_spares_just_inside_the_boundary() {
        let mut first_seen: HashMap<u32, Instant> = HashMap::new();
        let now = Instant::now();
        assert!(!grace_admits(&mut first_seen, 4321, now));
        let nearly = now + Duration::from_millis(orphan_reaper::ORPHAN_GRACE_MS - 1);
        assert!(!grace_admits(&mut first_seen, 4321, nearly));
    }

    /// Each PID is timed independently: an old candidate being admitted must
    /// not drag a freshly-seen sibling in with it.
    #[test]
    fn grace_window_times_each_pid_independently() {
        let mut first_seen: HashMap<u32, Instant> = HashMap::new();
        let now = Instant::now();
        assert!(!grace_admits(&mut first_seen, 100, now));
        let later = now + Duration::from_millis(orphan_reaper::ORPHAN_GRACE_MS);
        assert!(grace_admits(&mut first_seen, 100, later));
        assert!(
            !grace_admits(&mut first_seen, 200, later),
            "pid 200 was first seen at `later`, so its own window has not elapsed"
        );
    }
}
