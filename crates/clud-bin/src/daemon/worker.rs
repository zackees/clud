use std::fs;
use std::io::{self, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use running_process_core::pty::NativePtyProcess;
use running_process_core::{
    Containment, NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode,
};

use crate::subprocess;
use crate::win_creation_flags::invisible_helper_creationflags;

use super::io_helpers::{child_env, read_json_file, write_json_file, write_json_line};
use super::paths::{session_snapshot_path, spec_path};
use super::process_utils::pid_is_alive;
use super::types::{
    SessionKind, SessionRuntime, SessionSnapshot, WorkerClientMessage, WorkerLaunchSpec,
    WorkerServerMessage, DEFAULT_BACKLOG_LIMIT_BYTES,
};
use super::worker_shared::WorkerShared;

pub(super) fn run_worker(
    state_dir: &Path,
    session_id: &str,
    daemon_pid: u32,
    spec_file: &Path,
) -> i32 {
    let spec = match read_json_file::<WorkerLaunchSpec>(spec_file) {
        Ok(spec) => spec,
        Err(err) => {
            eprintln!("[clud] failed to read worker spec: {}", err);
            return 1;
        }
    };
    if spec.repeat_run_command.is_some() {
        return run_repeat_worker(state_dir, session_id, daemon_pid, &spec);
    }
    let listener = match TcpListener::bind(("127.0.0.1", 0)) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("[clud] failed to bind worker listener: {}", err);
            return 1;
        }
    };
    let _ = listener.set_nonblocking(true);
    let worker_port = match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(err) => {
            eprintln!("[clud] failed to read worker listener addr: {}", err);
            return 1;
        }
    };

    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let snapshot = SessionSnapshot {
        id: session_id.to_string(),
        kind: spec.kind.clone(),
        cwd: spec.plan.cwd.clone(),
        name: spec.name.clone(),
        created_at: Some(created_at),
        detachable: spec.detachable,
        background: spec.background_on_launch,
        attachable: spec.attachable,
        repeat_interval_secs: spec.repeat_interval_secs,
        repeat_next_run_at: None,
        repeat_running: spec.repeat_interval_secs.is_some(),
        daemon_pid,
        worker_pid: std::process::id(),
        worker_port,
        root_pid: None,
        exit_code: None,
    };
    let backlog_limit = spec.backlog_bytes.unwrap_or(DEFAULT_BACKLOG_LIMIT_BYTES);
    let shared = Arc::new(WorkerShared::new_with_backlog(
        state_dir.to_path_buf(),
        session_id.to_string(),
        snapshot,
        backlog_limit,
    ));
    shared.init_log_file();

    let runtime = match spec.kind {
        SessionKind::Subprocess => match start_subprocess_session(&spec, &shared) {
            Ok(runtime) => runtime,
            Err(err) => {
                eprintln!("[clud] failed to start subprocess session: {}", err);
                return 1;
            }
        },
        SessionKind::Pty => match start_pty_session(&spec, &shared) {
            Ok(runtime) => runtime,
            Err(err) => {
                eprintln!("[clud] failed to start PTY session: {}", err);
                return 1;
            }
        },
    };

    shared.set_root_pid(runtime.root_pid());
    if let Err(err) = persist_snapshot(state_dir, session_id, &shared) {
        eprintln!("[clud] failed to write session metadata: {}", err);
        return 1;
    }

    {
        let shared = Arc::clone(&shared);
        let runtime = runtime.clone();
        let state_dir = state_dir.to_path_buf();
        let session_id = session_id.to_string();
        thread::spawn(move || loop {
            if shared.snapshot().exit_code.is_some() {
                break;
            }
            if !pid_is_alive(daemon_pid) {
                runtime.cleanup_tree();
                shared.broadcast_exit(137);
                let _ = persist_snapshot(&state_dir, &session_id, &shared);
                let _ = fs::remove_file(spec_path(&state_dir, &session_id));
                break;
            }
            thread::sleep(Duration::from_millis(200));
        });
    }

    // Heartbeat thread: periodically probe the attached client's TCP connection.
    // If the peer has disconnected (e.g. terminal crash, SSH drop), evict the
    // dead client so new attach attempts succeed immediately.
    {
        let shared = Arc::clone(&shared);
        thread::spawn(move || loop {
            if shared.stop_accepting.load(Ordering::Acquire) {
                break;
            }
            shared.evict_dead_client();
            thread::sleep(Duration::from_secs(2));
        });
    }

    loop {
        if shared.stop_accepting.load(Ordering::Acquire) && !shared.has_client() {
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let shared = Arc::clone(&shared);
                let runtime = runtime.clone();
                thread::spawn(move || {
                    let _ = handle_worker_client(stream, &shared, &runtime);
                });
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(25));
            }
            Err(_) => break,
        }
    }
    let _ = persist_snapshot(state_dir, session_id, &shared);
    let _ = fs::remove_file(spec_path(state_dir, session_id));
    0
}

fn run_repeat_worker(
    state_dir: &Path,
    session_id: &str,
    daemon_pid: u32,
    spec: &WorkerLaunchSpec,
) -> i32 {
    let repeat_interval_secs = spec.repeat_interval_secs.unwrap_or(0);
    let repeat_run_command = spec.repeat_run_command.clone().unwrap_or_default();
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let snapshot = SessionSnapshot {
        id: session_id.to_string(),
        kind: SessionKind::Subprocess,
        cwd: spec.plan.cwd.clone(),
        name: spec.name.clone(),
        created_at: Some(created_at),
        detachable: false,
        background: true,
        attachable: false,
        repeat_interval_secs: Some(repeat_interval_secs),
        repeat_next_run_at: None,
        repeat_running: true,
        daemon_pid,
        worker_pid: std::process::id(),
        worker_port: 0,
        root_pid: None,
        exit_code: None,
    };
    let shared = Arc::new(WorkerShared::new_with_backlog(
        state_dir.to_path_buf(),
        session_id.to_string(),
        snapshot,
        spec.backlog_bytes.unwrap_or(DEFAULT_BACKLOG_LIMIT_BYTES),
    ));
    shared.init_log_file();
    if let Err(err) = persist_snapshot(state_dir, session_id, &shared) {
        eprintln!("[clud] failed to write repeat session metadata: {}", err);
        return 1;
    }

    loop {
        if !pid_is_alive(daemon_pid) {
            shared.set_exit_code(137);
            let _ = persist_snapshot(state_dir, session_id, &shared);
            let _ = fs::remove_file(spec_path(state_dir, session_id));
            return 0;
        }

        shared.set_repeat_state(true, None);
        if !run_repeat_once(&repeat_run_command, spec, daemon_pid, &shared) {
            let _ = persist_snapshot(state_dir, session_id, &shared);
            let _ = fs::remove_file(spec_path(state_dir, session_id));
            return 0;
        }
        shared.set_root_pid(None);

        let next_run_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
            + repeat_interval_secs.saturating_mul(1000);
        shared.set_repeat_state(false, Some(next_run_at));

        while (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64)
            < next_run_at
        {
            if !pid_is_alive(daemon_pid) {
                shared.set_exit_code(137);
                let _ = persist_snapshot(state_dir, session_id, &shared);
                let _ = fs::remove_file(spec_path(state_dir, session_id));
                return 0;
            }
            thread::sleep(Duration::from_millis(250));
        }
    }
}

fn run_repeat_once(
    command: &[String],
    spec: &WorkerLaunchSpec,
    daemon_pid: u32,
    shared: &Arc<WorkerShared>,
) -> bool {
    let process = Arc::new(NativeProcess::new(ProcessConfig {
        command: subprocess::command_spec_for_subprocess(command.to_vec()),
        cwd: spec.plan.cwd.as_ref().map(PathBuf::from),
        env: Some(child_env()),
        capture: true,
        stderr_mode: StderrMode::Stdout,
        // Issue #55: repeat-job runs are invisible by design — stdio is
        // captured into a TCP-broadcast log, the user never sees the
        // child's console directly. Suppress the conhost window on
        // Windows so each scheduled run doesn't pop a flash. No-op
        // elsewhere.
        creationflags: invisible_helper_creationflags(),
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
        containment: Some(Containment::Contained),
    }));
    if let Err(err) = process.start() {
        shared
            .push_output(format!("[clud repeat] failed to start child run: {err}\n").into_bytes());
        return true;
    }
    shared.set_root_pid(process.pid());

    loop {
        if !pid_is_alive(daemon_pid) {
            let _ = process.kill();
            let _ = process.wait(Some(Duration::from_secs(2)));
            shared.set_exit_code(137);
            return false;
        }
        match process.read_combined(Some(Duration::from_millis(100))) {
            ReadStatus::Line(event) => {
                let mut chunk = event.line;
                chunk.push(b'\n');
                shared.push_output(chunk);
            }
            ReadStatus::Timeout => {
                if process.returncode().is_some() {
                    break;
                }
            }
            ReadStatus::Eof => {
                if process.returncode().is_some() {
                    break;
                }
            }
        }
    }
    let _ = process.wait(Some(Duration::from_secs(2)));
    true
}

fn start_subprocess_session(
    spec: &WorkerLaunchSpec,
    shared: &Arc<WorkerShared>,
) -> io::Result<SessionRuntime> {
    let process = Arc::new(NativeProcess::new(ProcessConfig {
        command: subprocess::command_spec_for_subprocess(spec.plan.command.clone()),
        cwd: spec.plan.cwd.as_ref().map(PathBuf::from),
        env: Some(child_env()),
        capture: true,
        stderr_mode: StderrMode::Stdout,
        // Issue #55: daemon-managed subprocess session — stdio is fully
        // piped and routed via TCP to attaching clients. The child's
        // console would never be the user's interaction surface, so
        // suppress the conhost window on Windows. No-op elsewhere.
        creationflags: invisible_helper_creationflags(),
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
        containment: Some(Containment::Contained),
    }));
    process
        .start()
        .map_err(|err| io::Error::other(err.to_string()))?;

    // Drain stdout in a dedicated thread. We must NOT broadcast the
    // backend's exit until this drain has fully completed, otherwise
    // a race lets the wait-thread enqueue `Exited` ahead of an
    // unflushed final `Output` chunk on the worker→client channel —
    // an attaching client then breaks on Exited and silently drops
    // the backend's last line of output. macOS-ARM hit this most
    // often in `test_attach_last` (PR #136); the equivalent flake on
    // other platforms is harder to trigger but the bug is pre-existing.
    let read_handle = {
        let process = Arc::clone(&process);
        let shared = Arc::clone(shared);
        thread::spawn(move || loop {
            match process.read_combined(Some(Duration::from_millis(100))) {
                ReadStatus::Line(event) => {
                    let mut chunk = event.line;
                    chunk.push(b'\n');
                    shared.push_output(chunk);
                }
                ReadStatus::Timeout => {
                    if process.returncode().is_some() {
                        break;
                    }
                }
                ReadStatus::Eof => break,
            }
        })
    };

    {
        let process = Arc::clone(&process);
        let shared = Arc::clone(shared);
        thread::spawn(move || {
            let code = match process.wait(None) {
                Ok(code) => code,
                Err(_) => return,
            };
            // Wait for the stdout drain to finish so every `push_output`
            // call has landed before we enqueue `Exited`. `read_combined`
            // polls with a 100ms timeout and rechecks `returncode()` on
            // each Timeout, so this join terminates within ~100ms.
            let _ = read_handle.join();
            shared.broadcast_exit(code);
        });
    }

    Ok(SessionRuntime::Subprocess(process))
}

fn start_pty_session(
    spec: &WorkerLaunchSpec,
    shared: &Arc<WorkerShared>,
) -> io::Result<SessionRuntime> {
    let process = Arc::new(
        NativePtyProcess::new(
            spec.plan.command.clone(),
            spec.plan.cwd.clone(),
            Some(child_env()),
            spec.rows,
            spec.cols,
            None,
        )
        .map_err(|err| io::Error::other(err.to_string()))?,
    );
    process.set_echo(false);
    // Start the terminal emulator at the same dims as the PTY so early output
    // (launch banners, first frame of a TUI) lands in the grid from byte 0.
    // Without this, a client that attaches before any resize happens would
    // see a repaint of an empty 0x0 grid.
    shared.init_capture(spec.rows, spec.cols);
    process
        .start_impl()
        .map_err(|err| io::Error::other(err.to_string()))?;

    // Same Output-vs-Exited race fix as `start_subprocess_session`:
    // join the PTY-read thread before broadcasting exit so the
    // final chunk can never be enqueued after `Exited`.
    let read_handle = {
        let process = Arc::clone(&process);
        let shared = Arc::clone(shared);
        thread::spawn(move || loop {
            match process.read_chunk_impl(Some(0.1)) {
                Ok(Some(chunk)) => {
                    shared.push_output(chunk);
                }
                Ok(None) => {
                    if process.wait_impl(Some(0.0)).is_ok() {
                        break;
                    }
                }
                Err(_) => break,
            }
        })
    };

    {
        let process = Arc::clone(&process);
        let shared = Arc::clone(shared);
        thread::spawn(move || {
            let code = match process.wait_impl(None) {
                Ok(code) => code,
                Err(_) => return,
            };
            let _ = read_handle.join();
            shared.broadcast_exit(code);
        });
    }

    Ok(SessionRuntime::Pty(process))
}

fn handle_worker_client(
    mut stream: TcpStream,
    shared: &Arc<WorkerShared>,
    runtime: &SessionRuntime,
) -> io::Result<()> {
    let reader_stream = stream.try_clone()?;
    reader_stream.set_read_timeout(Some(Duration::from_millis(250)))?;
    let mut reader = BufReader::new(reader_stream);
    let mut line = String::new();
    if read_worker_line(&mut reader, &mut line, None)? == 0 {
        return Ok(());
    }
    let message: WorkerClientMessage = serde_json::from_str(&line)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    if !matches!(message, WorkerClientMessage::Attach) {
        return write_json_line(
            &mut stream,
            &WorkerServerMessage::Error {
                message: "expected attach handshake".to_string(),
            },
        );
    }

    let shutdown_handle = stream.try_clone()?;
    let (client_id, rx, snapshot, backlog) = match shared.attach_client(shutdown_handle) {
        Ok(values) => values,
        Err(message) => {
            return write_json_line(&mut stream, &WorkerServerMessage::Error { message });
        }
    };
    let mut writer = stream.try_clone()?;
    write_json_line(
        &mut writer,
        &WorkerServerMessage::Attached {
            session: snapshot.clone(),
        },
    )?;
    for chunk in backlog {
        write_json_line(
            &mut writer,
            &WorkerServerMessage::Output {
                data_b64: base64::engine::general_purpose::STANDARD.encode(chunk),
            },
        )?;
    }
    if let Some(exit_code) = snapshot.exit_code {
        write_json_line(&mut writer, &WorkerServerMessage::Exited { exit_code })?;
        shared.detach_client(client_id);
        return Ok(());
    }

    let shared_for_writer = Arc::clone(shared);
    let writer_thread = thread::spawn(move || {
        while let Ok(message) = rx.recv() {
            if write_json_line(&mut writer, &message).is_err() {
                break;
            }
        }
        shared_for_writer.detach_client(client_id);
    });

    loop {
        let mut line = String::new();
        match read_worker_line(&mut reader, &mut line, Some((shared, client_id)))? {
            0 => break,
            _ => {
                if !shared.owns_client(client_id) {
                    break;
                }
                let Ok(message) = serde_json::from_str::<WorkerClientMessage>(&line) else {
                    continue;
                };
                match message {
                    WorkerClientMessage::Attach => break,
                    WorkerClientMessage::Input { data_b64, submit } => {
                        if let Ok(data) =
                            base64::engine::general_purpose::STANDARD.decode(data_b64.as_bytes())
                        {
                            runtime.write(&data, submit);
                        }
                    }
                    WorkerClientMessage::Resize { rows, cols } => {
                        runtime.resize(rows, cols);
                        shared.resize_capture(rows, cols);
                    }
                    WorkerClientMessage::Interrupt => runtime.interrupt(),
                }
            }
        }
    }

    shared.detach_client(client_id);
    let _ = writer_thread.join();
    Ok(())
}

fn read_worker_line(
    reader: &mut BufReader<TcpStream>,
    line: &mut String,
    active_client: Option<(&Arc<WorkerShared>, u64)>,
) -> io::Result<usize> {
    use std::io::BufRead;
    loop {
        line.clear();
        match reader.read_line(line) {
            Ok(read) => return Ok(read),
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                if active_client.is_some_and(|(shared, client_id)| !shared.owns_client(client_id)) {
                    return Ok(0);
                }
            }
            Err(err) => return Err(err),
        }
    }
}

fn persist_snapshot(
    state_dir: &Path,
    session_id: &str,
    shared: &Arc<WorkerShared>,
) -> io::Result<()> {
    write_json_file(
        &session_snapshot_path(state_dir, session_id),
        &shared.snapshot(),
    )
}

// Silence import warnings for items consumed only by the `Write` trait or
// other macros above (none here currently).
#[allow(unused_imports)]
use Write as _;
