use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use running_process_core::pty::NativePtyProcess;
use running_process_core::{
    CommandSpec, Containment, NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode,
};
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, Signal, System};

use crate::args::{Args, Command};
use crate::backend::LaunchMode;
use crate::command::LaunchPlan;
use crate::trampoline;

const ENV_FEATURE_FLAG: &str = "CLUD_EXPERIMENTAL_DAEMON";
const ENV_STATE_DIR: &str = "CLUD_DAEMON_STATE_DIR";
const BACKLOG_LIMIT_BYTES: usize = 256 * 1024;
const STALE_CLIENT_GRACE: Duration = Duration::from_secs(1);
const BACKGROUND_PROMPT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SessionKind {
    Subprocess,
    Pty,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DaemonInfo {
    pid: u32,
    port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionSnapshot {
    id: String,
    kind: SessionKind,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    created_at: Option<u64>,
    #[serde(default)]
    detachable: bool,
    #[serde(default)]
    background: bool,
    daemon_pid: u32,
    worker_pid: u32,
    worker_port: u16,
    root_pid: Option<u32>,
    exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkerLaunchSpec {
    plan: LaunchPlan,
    kind: SessionKind,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    detachable: bool,
    #[serde(default)]
    background_on_launch: bool,
    rows: u16,
    cols: u16,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum DaemonRequest {
    Create { spec: WorkerLaunchSpec },
    Session { session_id: String },
    Terminate { session_id: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum DaemonResponse {
    Created { session: SessionSnapshot },
    Session { session: SessionSnapshot },
    Terminated { session: SessionSnapshot },
    Error { message: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum WorkerClientMessage {
    Attach,
    Input { data_b64: String, submit: bool },
    Resize { rows: u16, cols: u16 },
    Interrupt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum WorkerServerMessage {
    Attached { session: SessionSnapshot },
    Output { data_b64: String },
    Exited { exit_code: i32 },
    Error { message: String },
}

#[derive(Clone)]
enum SessionRuntime {
    Subprocess(Arc<NativeProcess>),
    Pty(Arc<NativePtyProcess>),
}

impl SessionRuntime {
    fn root_pid(&self) -> Option<u32> {
        match self {
            Self::Subprocess(process) => process.pid(),
            Self::Pty(process) => process.pid().ok().flatten(),
        }
    }

    fn interrupt(&self) {
        match self {
            Self::Subprocess(process) => {
                let _ = process.kill();
            }
            Self::Pty(process) => {
                let _ = process.send_interrupt_impl();
            }
        }
    }

    fn write(&self, data: &[u8], submit: bool) {
        if let Self::Pty(process) = self {
            let _ = process.write_impl(data, submit);
        }
    }

    fn resize(&self, rows: u16, cols: u16) {
        if let Self::Pty(process) = self {
            let _ = process.resize_impl(rows, cols);
        }
    }

    fn cleanup_tree(&self) {
        if let Some(pid) = self.root_pid() {
            signal_process_tree(pid, Signal::Term);
            thread::sleep(Duration::from_millis(150));
            signal_process_tree(pid, Signal::Kill);
        }
        match self {
            Self::Subprocess(process) => {
                let _ = process.kill();
            }
            Self::Pty(process) => {
                let _ = process.terminate_tree_impl();
                thread::sleep(Duration::from_millis(150));
                let _ = process.kill_tree_impl();
                let _ = process.close_impl();
            }
        }
    }
}

#[derive(Default)]
struct BacklogState {
    chunks: VecDeque<Vec<u8>>,
    total_bytes: usize,
}

struct AttachedClient {
    id: u64,
    sender: mpsc::Sender<WorkerServerMessage>,
    shutdown: TcpStream,
    attached_at: Instant,
}

type AttachClientResult = (
    u64,
    mpsc::Receiver<WorkerServerMessage>,
    SessionSnapshot,
    Vec<Vec<u8>>,
);

struct WorkerShared {
    state_dir: PathBuf,
    session_id: String,
    snapshot: Mutex<SessionSnapshot>,
    backlog: Mutex<BacklogState>,
    client: Mutex<Option<AttachedClient>>,
    next_client_id: AtomicU64,
    stop_accepting: AtomicBool,
}

impl WorkerShared {
    fn new(state_dir: PathBuf, session_id: String, snapshot: SessionSnapshot) -> Self {
        Self {
            state_dir,
            session_id,
            snapshot: Mutex::new(snapshot),
            backlog: Mutex::new(BacklogState::default()),
            client: Mutex::new(None),
            next_client_id: AtomicU64::new(1),
            stop_accepting: AtomicBool::new(false),
        }
    }

    fn snapshot(&self) -> SessionSnapshot {
        self.snapshot
            .lock()
            .expect("snapshot mutex poisoned")
            .clone()
    }

    fn set_root_pid(&self, root_pid: Option<u32>) {
        let snapshot = {
            let mut guard = self.snapshot.lock().expect("snapshot mutex poisoned");
            guard.root_pid = root_pid;
            guard.clone()
        };
        let _ = self.persist_snapshot(&snapshot);
    }

    fn set_exit_code(&self, exit_code: i32) {
        let snapshot = {
            let mut guard = self.snapshot.lock().expect("snapshot mutex poisoned");
            guard.exit_code = Some(exit_code);
            guard.clone()
        };
        let _ = self.persist_snapshot(&snapshot);
    }

    fn set_background(&self, background: bool) {
        let snapshot = {
            let mut guard = self.snapshot.lock().expect("snapshot mutex poisoned");
            if guard.background == background {
                return;
            }
            guard.background = background;
            guard.clone()
        };
        let _ = self.persist_snapshot(&snapshot);
    }

    fn attach_client(&self, shutdown: TcpStream) -> Result<AttachClientResult, String> {
        // First, try to evict any dead client before checking occupancy.
        self.evict_dead_client();
        let mut guard = self.client.lock().expect("client mutex poisoned");
        if guard
            .as_ref()
            .is_some_and(|client| client.attached_at.elapsed() < STALE_CLIENT_GRACE)
        {
            return Err("session already has an attached client".to_string());
        }
        let (tx, rx) = mpsc::channel();
        let client_id = self.next_client_id.fetch_add(1, Ordering::AcqRel);
        let previous = guard.replace(AttachedClient {
            id: client_id,
            sender: tx,
            shutdown,
            attached_at: Instant::now(),
        });
        drop(guard);
        if let Some(previous) = previous {
            let _ = previous.shutdown.shutdown(Shutdown::Both);
        }
        self.set_background(false);
        let snapshot = self.snapshot();
        let backlog = self
            .backlog
            .lock()
            .expect("backlog mutex poisoned")
            .chunks
            .iter()
            .cloned()
            .collect();
        Ok((client_id, rx, snapshot, backlog))
    }

    fn detach_client(&self, client_id: u64) {
        let mut guard = self.client.lock().expect("client mutex poisoned");
        if guard.as_ref().is_some_and(|client| client.id == client_id) {
            *guard = None;
        }
        drop(guard);
        if self.snapshot().exit_code.is_none() {
            self.set_background(true);
        }
    }

    fn owns_client(&self, client_id: u64) -> bool {
        self.client
            .lock()
            .expect("client mutex poisoned")
            .as_ref()
            .is_some_and(|client| client.id == client_id)
    }

    fn has_client(&self) -> bool {
        self.client.lock().expect("client mutex poisoned").is_some()
    }

    /// Check if the attached client's TCP connection is still alive.
    /// If the peer has disconnected, evict the dead client so new attaches succeed.
    fn evict_dead_client(&self) {
        let mut guard = self.client.lock().expect("client mutex poisoned");
        let should_evict = if let Some(client) = guard.as_ref() {
            // Try a zero-byte peek to check if the connection is still alive.
            // A connection-reset or broken-pipe error means the peer is gone.
            let mut probe = [0u8; 1];
            client.shutdown.set_nonblocking(true).ok();
            let dead = match client.shutdown.peek(&mut probe) {
                // EOF means peer closed the connection
                Ok(0) => true,
                // WouldBlock means the socket is alive but has no data
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => false,
                Err(ref e) if e.kind() == io::ErrorKind::ConnectionReset => true,
                Err(ref e) if e.kind() == io::ErrorKind::ConnectionAborted => true,
                // Unknown error: consider stale after 10s
                Err(_) => client.attached_at.elapsed() > Duration::from_secs(10),
                // Data available means socket is alive
                Ok(_) => false,
            };
            client.shutdown.set_nonblocking(false).ok();
            dead
        } else {
            false
        };
        if should_evict {
            if let Some(old) = guard.take() {
                let _ = old.shutdown.shutdown(Shutdown::Both);
            }
            drop(guard);
            if self.snapshot().exit_code.is_none() {
                self.set_background(true);
            }
        }
    }

    fn push_output(&self, chunk: Vec<u8>) {
        {
            let mut backlog = self.backlog.lock().expect("backlog mutex poisoned");
            backlog.total_bytes += chunk.len();
            backlog.chunks.push_back(chunk.clone());
            while backlog.total_bytes > BACKLOG_LIMIT_BYTES {
                if let Some(front) = backlog.chunks.pop_front() {
                    backlog.total_bytes = backlog.total_bytes.saturating_sub(front.len());
                } else {
                    break;
                }
            }
        }
        self.send_to_client(WorkerServerMessage::Output {
            data_b64: base64::engine::general_purpose::STANDARD.encode(chunk),
        });
    }

    fn broadcast_exit(&self, exit_code: i32) {
        self.set_exit_code(exit_code);
        self.stop_accepting.store(true, Ordering::Release);
        self.send_to_client(WorkerServerMessage::Exited { exit_code });
    }

    fn send_to_client(&self, message: WorkerServerMessage) {
        let sender = self
            .client
            .lock()
            .expect("client mutex poisoned")
            .as_ref()
            .map(|client| client.sender.clone());
        if let Some(sender) = sender {
            let _ = sender.send(message);
        }
    }

    fn persist_snapshot(&self, snapshot: &SessionSnapshot) -> io::Result<()> {
        write_json_file(
            &session_snapshot_path(&self.state_dir, &self.session_id),
            snapshot,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalAttachResult {
    Completed(i32),
    InterruptRequested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackgroundPromptDecision {
    ContinueInBackground,
    EndSession,
}

struct RawTerminalGuard;

impl RawTerminalGuard {
    fn enter() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawTerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

pub fn experimental_enabled(args: &Args) -> bool {
    args.detach
        || args.detachable
        || args.experimental_daemon_centralized
        || std::env::var(ENV_FEATURE_FLAG)
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
}

pub fn handle_special_command(args: &Args, interrupted: &AtomicBool) -> Option<i32> {
    match &args.command {
        Some(Command::Attach {
            session_id: Some(session_id),
            last,
        }) if !last => {
            let state_dir = state_dir(args);
            if session_id == "-" {
                // "clud attach -" is shorthand for --last
                match most_recent_session(&state_dir) {
                    Some(session) => {
                        eprintln!("[clud] attaching to most recent session: {}", session.id);
                        Some(run_attach(&session.id, &state_dir, interrupted))
                    }
                    None => {
                        println!("No active sessions.");
                        Some(0)
                    }
                }
            } else {
                Some(run_attach(session_id, &state_dir, interrupted))
            }
        }
        Some(Command::Attach { last: true, .. }) => {
            let state_dir = state_dir(args);
            match most_recent_session(&state_dir) {
                Some(session) => {
                    eprintln!("[clud] attaching to most recent session: {}", session.id);
                    Some(run_attach(&session.id, &state_dir, interrupted))
                }
                None => {
                    println!("No active sessions.");
                    Some(0)
                }
            }
        }
        Some(Command::Attach {
            session_id: None,
            last: false,
        }) => {
            let state_dir = state_dir(args);
            let sessions = list_attachable_sessions(&state_dir);
            if sessions.is_empty() {
                println!("No active sessions.");
                println!("Start one with: clud --detach -p <prompt>");
                Some(0)
            } else if sessions.len() == 1 {
                eprintln!("[clud] auto-attaching to only session: {}", sessions[0].id);
                Some(run_attach(&sessions[0].id, &state_dir, interrupted))
            } else {
                Some(run_list(&state_dir))
            }
        }
        Some(Command::Kill { session_id, all }) => {
            let state_dir = state_dir(args);
            Some(run_kill(&state_dir, session_id.as_deref(), *all))
        }
        Some(Command::List) => {
            let state_dir = state_dir(args);
            Some(run_list(&state_dir))
        }
        Some(Command::InternalDaemon { state_dir }) => Some(run_daemon(state_dir)),
        Some(Command::InternalWorker {
            state_dir,
            session_id,
            daemon_pid,
            spec_file,
        }) => Some(run_worker(state_dir, session_id, *daemon_pid, spec_file)),
        _ => None,
    }
}

pub fn run_centralized_session(args: &Args, plan: &LaunchPlan, interrupted: &AtomicBool) -> i32 {
    let state_dir = state_dir(args);
    if let Err(err) = ensure_daemon(&state_dir) {
        eprintln!("[clud] failed to start daemon: {}", err);
        return 1;
    }

    let kind = match plan.launch_mode {
        LaunchMode::Subprocess => SessionKind::Subprocess,
        LaunchMode::Pty => SessionKind::Pty,
    };
    let (rows, cols) = terminal_dimensions();
    let request = DaemonRequest::Create {
        spec: WorkerLaunchSpec {
            plan: plan.clone(),
            kind,
            name: args.session_name.clone(),
            detachable: args.detach || args.detachable,
            background_on_launch: args.detach,
            rows,
            cols,
        },
    };
    let response = match send_daemon_request(&state_dir, &request) {
        Ok(response) => response,
        Err(err) => {
            eprintln!("[clud] daemon request failed: {}", err);
            return 1;
        }
    };

    match response {
        DaemonResponse::Created { session } => {
            if args.detach {
                eprintln!("[clud] session {} running in background", session.id);
                eprintln!("[clud] attach with: clud attach {}", session.id);
                return 0;
            }
            eprintln!("[clud] daemon session {}", session.id);
            {
                attach_to_session(&state_dir, &session, interrupted)
            }
        }
        DaemonResponse::Error { message } => {
            eprintln!("[clud] daemon error: {}", message);
            1
        }
        DaemonResponse::Session { .. } | DaemonResponse::Terminated { .. } => 1,
    }
}

fn run_attach(session_id: &str, state_dir: &Path, interrupted: &AtomicBool) -> i32 {
    if let Err(err) = ensure_daemon(state_dir) {
        eprintln!("[clud] daemon is not running: {}", err);
        eprintln!("[clud] start a session with: clud --detach -p <prompt>");
        return 1;
    }
    let resolved = match resolve_session_id(state_dir, session_id) {
        Ok(id) => id,
        Err(err) => {
            eprintln!("[clud] {}", err);
            return 1;
        }
    };
    let response = match send_daemon_request(
        state_dir,
        &DaemonRequest::Session {
            session_id: resolved.clone(),
        },
    ) {
        Ok(response) => response,
        Err(err) => {
            eprintln!("[clud] failed to query session {}: {}", session_id, err);
            return 1;
        }
    };
    match response {
        DaemonResponse::Session { session } => attach_to_session(state_dir, &session, interrupted),
        DaemonResponse::Error { message } => {
            eprintln!("[clud] daemon error: {}", message);
            1
        }
        DaemonResponse::Created { .. } | DaemonResponse::Terminated { .. } => 1,
    }
}

fn attach_to_session(state_dir: &Path, session: &SessionSnapshot, interrupted: &AtomicBool) -> i32 {
    let started = Instant::now();
    let attach_retry_window = Duration::from_secs(5);
    let (writer, mut reader) = loop {
        let mut stream = match TcpStream::connect(("127.0.0.1", session.worker_port)) {
            Ok(stream) => stream,
            Err(err) => {
                if !pid_is_alive(session.worker_pid) {
                    eprintln!(
                        "[clud] session {} worker has died (pid {})",
                        session.id, session.worker_pid
                    );
                } else {
                    eprintln!(
                        "[clud] failed to connect to session {} worker on port {}: {}",
                        session.id, session.worker_port, err
                    );
                }
                return 1;
            }
        };
        if let Err(err) = write_json_line(&mut stream, &WorkerClientMessage::Attach) {
            eprintln!("[clud] failed to attach to session {}: {}", session.id, err);
            return 1;
        }

        let writer = match stream.try_clone() {
            Ok(writer) => Arc::new(Mutex::new(writer)),
            Err(err) => {
                eprintln!("[clud] failed to clone session writer: {}", err);
                return 1;
            }
        };
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                eprintln!(
                    "[clud] daemon worker closed the connection for session {}",
                    session.id
                );
                return 1;
            }
            Ok(_) => {}
            Err(err) => {
                eprintln!("[clud] failed to attach to session {}: {}", session.id, err);
                return 1;
            }
        }

        let message = match serde_json::from_str::<WorkerServerMessage>(&line) {
            Ok(message) => message,
            Err(err) => {
                eprintln!(
                    "[clud] invalid worker response for session {}: {}",
                    session.id, err
                );
                return 1;
            }
        };
        match message {
            WorkerServerMessage::Attached { .. } => break (writer, reader),
            WorkerServerMessage::Error { message }
                if message == "session already has an attached client"
                    && started.elapsed() < attach_retry_window =>
            {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            WorkerServerMessage::Error { message } => {
                eprintln!("[clud] {}", message);
                return 1;
            }
            WorkerServerMessage::Exited { exit_code } => return exit_code,
            WorkerServerMessage::Output { data_b64 } => {
                if let Ok(bytes) =
                    base64::engine::general_purpose::STANDARD.decode(data_b64.as_bytes())
                {
                    let _ = io::stdout().write_all(&bytes);
                    let _ = io::stdout().flush();
                }
                eprintln!(
                    "[clud] daemon worker sent output before attach handshake for session {}",
                    session.id
                );
                return 1;
            }
        }
    };

    let exit_code = Arc::new(Mutex::new(None));
    let reader_exit = Arc::clone(&exit_code);
    let reader = thread::spawn(move || loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let Ok(message) = serde_json::from_str::<WorkerServerMessage>(&line) else {
                    continue;
                };
                match message {
                    WorkerServerMessage::Attached { .. } => {}
                    WorkerServerMessage::Output { data_b64 } => {
                        if let Ok(bytes) =
                            base64::engine::general_purpose::STANDARD.decode(data_b64.as_bytes())
                        {
                            let _ = io::stdout().write_all(&bytes);
                            let _ = io::stdout().flush();
                        }
                    }
                    WorkerServerMessage::Exited { exit_code } => {
                        *reader_exit.lock().expect("exit code mutex poisoned") = Some(exit_code);
                        break;
                    }
                    WorkerServerMessage::Error { message } => {
                        let _ = writeln!(io::stderr(), "[clud] {}", message);
                        *reader_exit.lock().expect("exit code mutex poisoned") = Some(1);
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    });

    let local_result = if matches!(session.kind, SessionKind::Pty)
        && io::stdin().is_terminal()
        && io::stdout().is_terminal()
    {
        run_remote_interactive(Arc::clone(&writer), interrupted, session.detachable)
    } else {
        wait_for_remote_or_interrupt(&exit_code, interrupted)
    };

    let (local_result, backgrounded) = match local_result {
        LocalAttachResult::Completed(code) => (code, false),
        LocalAttachResult::InterruptRequested => {
            interrupted.store(false, Ordering::SeqCst);
            if session.detachable {
                match prompt_continue_in_background(interrupted) {
                    BackgroundPromptDecision::ContinueInBackground => {
                        let _ = shutdown_worker_connection(&writer);
                        eprintln!("[clud] session {} continues in the background", session.id);
                        (0, true)
                    }
                    BackgroundPromptDecision::EndSession => {
                        eprintln!("[clud] ending session {}", session.id);
                        let _ = request_session_termination(state_dir, &session.id);
                        let _ = shutdown_worker_connection(&writer);
                        (130, false)
                    }
                }
            } else {
                let _ = send_worker_message(&writer, &WorkerClientMessage::Interrupt);
                wait_for_remote_exit(&exit_code, Duration::from_secs(5));
                let _ = shutdown_worker_connection(&writer);
                (130, false)
            }
        }
    };

    if backgrounded {
        let _ = shutdown_worker_connection(&writer);
    }
    let _ = reader.join();
    if local_result == 130 {
        return 130;
    }
    let final_exit_code = exit_code
        .lock()
        .expect("exit code mutex poisoned")
        .unwrap_or(local_result);
    final_exit_code
}

fn run_remote_interactive(
    writer: Arc<Mutex<TcpStream>>,
    interrupted: &AtomicBool,
    _detachable: bool,
) -> LocalAttachResult {
    let _guard = match RawTerminalGuard::enter() {
        Ok(guard) => guard,
        Err(err) => {
            eprintln!(
                "[clud] warning: failed to enable raw terminal mode: {}",
                err
            );
            return LocalAttachResult::Completed(1);
        }
    };
    loop {
        if interrupted.load(Ordering::SeqCst) {
            return LocalAttachResult::InterruptRequested;
        }
        match event::poll(Duration::from_millis(25)) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) => match translate_key_event(key) {
                    KeyAction::Forward(bytes) => {
                        let submit = bytes == b"\r";
                        let _ = send_worker_message(
                            &writer,
                            &WorkerClientMessage::Input {
                                data_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
                                submit,
                            },
                        );
                    }
                    KeyAction::Interrupt => {
                        return LocalAttachResult::InterruptRequested;
                    }
                    KeyAction::Ignore => {}
                },
                Ok(Event::Paste(text)) => {
                    let _ = send_worker_message(
                        &writer,
                        &WorkerClientMessage::Input {
                            data_b64: base64::engine::general_purpose::STANDARD
                                .encode(text.as_bytes()),
                            submit: false,
                        },
                    );
                }
                Ok(Event::Resize(cols, rows)) => {
                    let _ =
                        send_worker_message(&writer, &WorkerClientMessage::Resize { rows, cols });
                }
                Ok(_) => {}
                Err(_) => return LocalAttachResult::Completed(1),
            },
            Ok(false) => {}
            Err(_) => return LocalAttachResult::Completed(1),
        }
    }
}

fn wait_for_remote_or_interrupt(
    exit_code: &Arc<Mutex<Option<i32>>>,
    interrupted: &AtomicBool,
) -> LocalAttachResult {
    while !interrupted.load(Ordering::SeqCst)
        && exit_code
            .lock()
            .expect("exit code mutex poisoned")
            .is_none()
    {
        thread::sleep(Duration::from_millis(25));
    }
    if interrupted.load(Ordering::SeqCst) {
        LocalAttachResult::InterruptRequested
    } else {
        LocalAttachResult::Completed(0)
    }
}

fn wait_for_remote_exit(exit_code: &Arc<Mutex<Option<i32>>>, timeout: Duration) {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if exit_code
            .lock()
            .expect("exit code mutex poisoned")
            .is_some()
        {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn prompt_continue_in_background(interrupted: &AtomicBool) -> BackgroundPromptDecision {
    if io::stdin().is_terminal() && io::stderr().is_terminal() {
        prompt_continue_in_background_terminal(interrupted)
    } else {
        prompt_continue_in_background_stream(interrupted)
    }
}

fn prompt_continue_in_background_terminal(interrupted: &AtomicBool) -> BackgroundPromptDecision {
    let _guard = match RawTerminalGuard::enter() {
        Ok(guard) => guard,
        Err(_) => return BackgroundPromptDecision::EndSession,
    };
    let started = Instant::now();
    let mut displayed_remaining = u64::MAX;
    loop {
        let remaining = BACKGROUND_PROMPT_TIMEOUT
            .as_secs()
            .saturating_sub(started.elapsed().as_secs());
        if remaining != displayed_remaining {
            displayed_remaining = remaining;
            render_background_prompt(remaining);
        }
        if remaining == 0 {
            eprintln!();
            return BackgroundPromptDecision::EndSession;
        }
        if interrupted.swap(false, Ordering::SeqCst) {
            eprintln!();
            return BackgroundPromptDecision::EndSession;
        }
        match event::poll(Duration::from_millis(100)) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) => match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        eprintln!();
                        return BackgroundPromptDecision::ContinueInBackground;
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        eprintln!();
                        return BackgroundPromptDecision::EndSession;
                    }
                    KeyCode::Enter | KeyCode::Esc => {
                        eprintln!();
                        return BackgroundPromptDecision::EndSession;
                    }
                    _ => {}
                },
                Ok(_) => {}
                Err(_) => {
                    eprintln!();
                    return BackgroundPromptDecision::EndSession;
                }
            },
            Ok(false) => {}
            Err(_) => {
                eprintln!();
                return BackgroundPromptDecision::EndSession;
            }
        }
    }
}

fn prompt_continue_in_background_stream(interrupted: &AtomicBool) -> BackgroundPromptDecision {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut stdin = io::stdin();
        let mut buf = [0u8; 1];
        if stdin.read(&mut buf).ok().is_some_and(|read| read > 0) {
            let _ = tx.send(buf[0]);
        }
    });

    let started = Instant::now();
    let mut displayed_remaining = u64::MAX;
    loop {
        let remaining = BACKGROUND_PROMPT_TIMEOUT
            .as_secs()
            .saturating_sub(started.elapsed().as_secs());
        if remaining != displayed_remaining {
            displayed_remaining = remaining;
            render_background_prompt(remaining);
        }
        if remaining == 0 {
            return BackgroundPromptDecision::EndSession;
        }
        if interrupted.swap(false, Ordering::SeqCst) {
            return BackgroundPromptDecision::EndSession;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(b'y') | Ok(b'Y') => return BackgroundPromptDecision::ContinueInBackground,
            Ok(_) => return BackgroundPromptDecision::EndSession,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return BackgroundPromptDecision::EndSession
            }
        }
    }
}

fn render_background_prompt(remaining: u64) {
    eprintln!(
        "[clud] continue session in the background? [y/N] auto-ending in {}s",
        remaining
    );
}

fn cleanup_stale_state(state_dir: &Path) {
    // Clean stale session files: mark sessions whose worker is dead.
    if let Ok(entries) = fs::read_dir(sessions_dir(state_dir)) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(mut session) = read_json_file::<SessionSnapshot>(&path) else {
                continue;
            };
            if session.exit_code.is_some() {
                continue;
            }
            if !pid_is_alive(session.worker_pid) {
                session.exit_code = Some(137);
                session.background = false;
                let _ = write_json_file(&path, &session);
            }
        }
    }

    // Clean dangling spec files: specs with no corresponding session snapshot
    // that are older than 10 seconds (grace period for worker startup).
    if let Ok(entries) = fs::read_dir(specs_dir(state_dir)) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let session_id = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            let snapshot_path = session_snapshot_path(state_dir, session_id);
            if snapshot_path.exists() {
                continue;
            }
            // Only remove if the spec is old enough (worker may still be starting).
            let is_stale = path
                .metadata()
                .and_then(|m| m.modified())
                .map(|modified| modified.elapsed().unwrap_or_default() > Duration::from_secs(10))
                .unwrap_or(true);
            if is_stale {
                let _ = fs::remove_file(&path);
            }
        }
    }

    // Clean stale daemon.json if it refers to a dead process.
    let daemon_path = daemon_info_path(state_dir);
    if let Ok(info) = read_json_file::<DaemonInfo>(&daemon_path) {
        if !pid_is_alive(info.pid) {
            let _ = fs::remove_file(&daemon_path);
        }
    }
}

fn run_daemon(state_dir: &Path) -> i32 {
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
    let info = DaemonInfo {
        pid: std::process::id(),
        port,
    };
    if let Err(err) = write_json_file(&daemon_info_path(state_dir), &info) {
        eprintln!("[clud] failed to persist daemon info: {}", err);
        return 1;
    }

    let workers = Arc::new(Mutex::new(HashMap::<String, Arc<NativeProcess>>::new()));
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        let workers = Arc::clone(&workers);
        let state_dir = state_dir.to_path_buf();
        thread::spawn(move || {
            let _ = handle_daemon_connection(stream, &state_dir, &workers);
        });
    }
    0
}

fn handle_daemon_connection(
    mut stream: TcpStream,
    state_dir: &Path,
    workers: &Arc<Mutex<HashMap<String, Arc<NativeProcess>>>>,
) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    let request: DaemonRequest = serde_json::from_str(&line)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    let response = match request {
        DaemonRequest::Create { spec } => match daemon_create_session(state_dir, workers, spec) {
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
        DaemonRequest::Terminate { session_id } => {
            match daemon_terminate_session(state_dir, workers, &session_id) {
                Ok(session) => DaemonResponse::Terminated { session },
                Err(err) => DaemonResponse::Error {
                    message: err.to_string(),
                },
            }
        }
    };
    write_json_line(&mut stream, &response)
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
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
        containment: Some(Containment::Detached),
    }));
    worker
        .start()
        .map_err(|err| io::Error::other(err.to_string()))?;

    let started = Instant::now();
    let snapshot = loop {
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
    // before reporting the session as ready.
    loop {
        if TcpStream::connect(("127.0.0.1", snapshot.worker_port)).is_ok() {
            break;
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

    workers
        .lock()
        .expect("workers mutex poisoned")
        .insert(session_id.clone(), worker);
    Ok(snapshot)
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

fn run_worker(state_dir: &Path, session_id: &str, daemon_pid: u32, spec_file: &Path) -> i32 {
    let spec = match read_json_file::<WorkerLaunchSpec>(spec_file) {
        Ok(spec) => spec,
        Err(err) => {
            eprintln!("[clud] failed to read worker spec: {}", err);
            return 1;
        }
    };
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
        daemon_pid,
        worker_pid: std::process::id(),
        worker_port,
        root_pid: None,
        exit_code: None,
    };
    let shared = Arc::new(WorkerShared::new(
        state_dir.to_path_buf(),
        session_id.to_string(),
        snapshot,
    ));

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

fn start_subprocess_session(
    spec: &WorkerLaunchSpec,
    shared: &Arc<WorkerShared>,
) -> io::Result<SessionRuntime> {
    use std::path::PathBuf;

    let process = Arc::new(NativeProcess::new(ProcessConfig {
        command: CommandSpec::Argv(spec.plan.command.clone()),
        cwd: spec.plan.cwd.as_ref().map(PathBuf::from),
        env: Some(child_env()),
        capture: true,
        stderr_mode: StderrMode::Stdout,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
        containment: Some(Containment::Contained),
    }));
    process
        .start()
        .map_err(|err| io::Error::other(err.to_string()))?;

    {
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
        });
    }

    {
        let process = Arc::clone(&process);
        let shared = Arc::clone(shared);
        thread::spawn(move || {
            if let Ok(code) = process.wait(None) {
                shared.broadcast_exit(code);
            }
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
    process
        .start_impl()
        .map_err(|err| io::Error::other(err.to_string()))?;

    {
        let process = Arc::clone(&process);
        let shared = Arc::clone(shared);
        thread::spawn(move || loop {
            match process.read_chunk_impl(Some(0.1)) {
                Ok(Some(chunk)) => {
                    // DSR auto-reply intentionally skipped — see issue #31 T1.
                    shared.push_output(chunk);
                }
                Ok(None) => {
                    if process.wait_impl(Some(0.0)).is_ok() {
                        break;
                    }
                }
                Err(_) => break,
            }
        });
    }

    {
        let process = Arc::clone(&process);
        let shared = Arc::clone(shared);
        thread::spawn(move || {
            if let Ok(code) = process.wait_impl(None) {
                shared.broadcast_exit(code);
            }
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
                    WorkerClientMessage::Resize { rows, cols } => runtime.resize(rows, cols),
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

fn ensure_daemon(state_dir: &Path) -> io::Result<()> {
    fs::create_dir_all(state_dir)?;
    cleanup_stale_state(state_dir);
    if let Ok(info) = read_json_file::<DaemonInfo>(&daemon_info_path(state_dir)) {
        if TcpStream::connect(("127.0.0.1", info.port)).is_ok() {
            return Ok(());
        }
    }

    let args = vec![
        "__daemon".to_string(),
        "--state-dir".to_string(),
        state_dir.to_string_lossy().to_string(),
    ];
    trampoline::spawn_detached_self(&args)?;

    let started = Instant::now();
    loop {
        if let Ok(info) = read_json_file::<DaemonInfo>(&daemon_info_path(state_dir)) {
            if TcpStream::connect(("127.0.0.1", info.port)).is_ok() {
                return Ok(());
            }
        }
        if started.elapsed() > Duration::from_secs(5) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for daemon startup",
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn send_daemon_request(state_dir: &Path, request: &DaemonRequest) -> io::Result<DaemonResponse> {
    let info = read_json_file::<DaemonInfo>(&daemon_info_path(state_dir))?;
    let mut stream = TcpStream::connect(("127.0.0.1", info.port))?;
    write_json_line(&mut stream, request)?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    serde_json::from_str(&line).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

fn request_session_termination(state_dir: &Path, session_id: &str) -> io::Result<SessionSnapshot> {
    match send_daemon_request(
        state_dir,
        &DaemonRequest::Terminate {
            session_id: session_id.to_string(),
        },
    )? {
        DaemonResponse::Terminated { session } => Ok(session),
        DaemonResponse::Error { message } => Err(io::Error::other(message)),
        response => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected daemon response: {response:?}"),
        )),
    }
}

fn send_worker_message(
    writer: &Arc<Mutex<TcpStream>>,
    message: &WorkerClientMessage,
) -> io::Result<()> {
    let mut guard = writer.lock().expect("writer mutex poisoned");
    write_json_line(&mut guard, message)
}

fn shutdown_worker_connection(writer: &Arc<Mutex<TcpStream>>) -> io::Result<()> {
    let guard = writer.lock().expect("writer mutex poisoned");
    guard.shutdown(Shutdown::Both)
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

fn child_env() -> Vec<(String, String)> {
    let originator_key = running_process_core::ORIGINATOR_ENV_VAR;
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(key, _)| key != "IN_CLUD" && key != originator_key)
        .collect();
    env.push(("IN_CLUD".to_string(), "1".to_string()));
    env.push((
        originator_key.to_string(),
        format!("CLUD:{}", std::process::id()),
    ));
    env
}

fn state_dir(args: &Args) -> PathBuf {
    if let Some(path) = &args.daemon_state_dir {
        return path.clone();
    }
    if let Ok(path) = std::env::var(ENV_STATE_DIR) {
        return PathBuf::from(path);
    }
    std::env::temp_dir().join("clud-daemon")
}

fn daemon_info_path(state_dir: &Path) -> PathBuf {
    state_dir.join("daemon.json")
}

fn sessions_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("sessions")
}

fn specs_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("specs")
}

fn session_snapshot_path(state_dir: &Path, session_id: &str) -> PathBuf {
    sessions_dir(state_dir).join(format!("{session_id}.json"))
}

fn spec_path(state_dir: &Path, session_id: &str) -> PathBuf {
    specs_dir(state_dir).join(format!("{session_id}.json"))
}

/// Resolve a user-provided session identifier to the canonical session ID.
/// Tries exact match, then name match, then prefix match.
fn resolve_session_id(state_dir: &Path, input: &str) -> Result<String, String> {
    // Exact match
    let exact_path = session_snapshot_path(state_dir, input);
    if exact_path.exists() {
        return Ok(input.to_string());
    }

    // Scan all sessions for name match or prefix match
    let Ok(entries) = fs::read_dir(sessions_dir(state_dir)) else {
        return Err(format!("session '{}' not found", input));
    };

    let mut name_matches = Vec::new();
    let mut prefix_matches = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(session) = read_json_file::<SessionSnapshot>(&path) else {
            continue;
        };
        if session.name.as_deref() == Some(input) {
            name_matches.push(session.id.clone());
        }
        if session.id.starts_with(input) {
            prefix_matches.push(session.id.clone());
        }
    }

    if name_matches.len() == 1 {
        return Ok(name_matches.into_iter().next().unwrap());
    }
    if name_matches.len() > 1 {
        return Err(format!(
            "ambiguous name '{}': matches {}",
            input,
            name_matches.join(", ")
        ));
    }
    if prefix_matches.len() == 1 {
        return Ok(prefix_matches.into_iter().next().unwrap());
    }
    if prefix_matches.len() > 1 {
        return Err(format!(
            "ambiguous prefix '{}': matches {}",
            input,
            prefix_matches.join(", ")
        ));
    }

    Err(format!("session '{}' not found", input))
}

/// Return the most recently created active session.
fn most_recent_session(state_dir: &Path) -> Option<SessionSnapshot> {
    let sessions = list_attachable_sessions(state_dir);
    sessions
        .into_iter()
        .max_by_key(|s| s.created_at.unwrap_or(0))
}

fn run_kill(state_dir: &Path, session_id: Option<&str>, all: bool) -> i32 {
    if let Err(err) = ensure_daemon(state_dir) {
        eprintln!("[clud] failed to reach daemon: {}", err);
        return 1;
    }

    if all {
        let sessions = list_attachable_sessions(state_dir);
        if sessions.is_empty() {
            println!("No active sessions to kill.");
            return 0;
        }
        let mut failed = 0;
        for session in &sessions {
            match request_session_termination(state_dir, &session.id) {
                Ok(_) => eprintln!("[clud] killed session {}", session.id),
                Err(err) => {
                    eprintln!("[clud] failed to kill session {}: {}", session.id, err);
                    failed += 1;
                }
            }
        }
        if failed > 0 {
            return 1;
        }
        return 0;
    }

    let Some(input) = session_id else {
        eprintln!("Usage: clud kill <session_id> or clud kill --all");
        return 1;
    };

    let resolved = match resolve_session_id(state_dir, input) {
        Ok(id) => id,
        Err(err) => {
            eprintln!("[clud] {}", err);
            return 1;
        }
    };

    match request_session_termination(state_dir, &resolved) {
        Ok(_) => {
            eprintln!("[clud] killed session {}", resolved);
            0
        }
        Err(err) => {
            eprintln!("[clud] failed to kill session {}: {}", resolved, err);
            1
        }
    }
}

fn format_duration_short(millis: u64) -> String {
    let now_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let elapsed_secs = now_millis.saturating_sub(millis) / 1000;
    if elapsed_secs < 60 {
        format!("{}s", elapsed_secs)
    } else if elapsed_secs < 3600 {
        format!("{}m", elapsed_secs / 60)
    } else {
        format!("{}h{}m", elapsed_secs / 3600, (elapsed_secs % 3600) / 60)
    }
}

fn run_list(state_dir: &Path) -> i32 {
    let sessions = list_attachable_sessions(state_dir);
    if sessions.is_empty() {
        println!("No background sessions.");
        return 0;
    }

    println!("{:<30} {:<8} {:<8} CWD", "SESSION", "PID", "UPTIME");
    for session in sessions {
        let display_name = session
            .name
            .as_deref()
            .map(|n| format!("{} ({})", session.id, n))
            .unwrap_or_else(|| session.id.clone());
        let pid = session
            .root_pid
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());
        let uptime = session
            .created_at
            .map(format_duration_short)
            .unwrap_or_else(|| "-".to_string());
        let cwd = session.cwd.unwrap_or_else(|| "-".to_string());
        println!("{:<30} {:<8} {:<8} {}", display_name, pid, uptime, cwd);
    }
    0
}

fn list_attachable_sessions(state_dir: &Path) -> Vec<SessionSnapshot> {
    let Ok(entries) = fs::read_dir(sessions_dir(state_dir)) else {
        return Vec::new();
    };
    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(session) = read_json_file::<SessionSnapshot>(&path) else {
            continue;
        };
        if session.exit_code.is_some() || !session.background {
            continue;
        }
        if !pid_is_alive(session.worker_pid) {
            continue;
        }
        if let Some(root_pid) = session.root_pid {
            if !pid_is_alive(root_pid) {
                continue;
            }
        }
        sessions.push(session);
    }
    sessions.sort_by(|left, right| left.id.cmp(&right.id));
    sessions
}

fn write_json_line<T: Serialize>(writer: &mut TcpStream, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn write_json_file<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing parent"))?;
    fs::create_dir_all(parent)?;
    let temp_path = path.with_extension("tmp");
    fs::write(
        &temp_path,
        serde_json::to_vec_pretty(value)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?,
    )?;
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    fs::rename(temp_path, path)
}

fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<T> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

fn new_session_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let sequence = COUNTER.fetch_add(1, Ordering::AcqRel);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("sess-{millis}-{sequence}")
}

fn terminal_dimensions() -> (u16, u16) {
    if let Some((width, height)) = terminal_size::terminal_size() {
        (height.0, width.0)
    } else {
        (24, 32767)
    }
}

fn pid_is_alive(pid: u32) -> bool {
    let system = System::new_all();
    system.process(Pid::from_u32(pid)).is_some()
}

fn signal_process_tree(root_pid: u32, signal: Signal) {
    let system = System::new_all();
    let root = Pid::from_u32(root_pid);
    if system.process(root).is_none() {
        return;
    }
    let mut descendants = descendant_pids(&system, root);
    descendants.reverse();
    descendants.push(root);
    for pid in descendants {
        if let Some(process) = system.process(pid) {
            let _ = process.kill_with(signal);
            if matches!(signal, Signal::Kill) {
                let _ = process.kill();
            }
        }
    }
}

fn descendant_pids(system: &System, root: Pid) -> Vec<Pid> {
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            children.entry(parent).or_default().push(*pid);
        }
    }
    let mut stack = vec![root];
    let mut descendants = Vec::new();
    while let Some(current) = stack.pop() {
        if let Some(next) = children.get(&current) {
            for child in next {
                descendants.push(*child);
                stack.push(*child);
            }
        }
    }
    descendants
}

#[derive(Debug, PartialEq, Eq)]
enum KeyAction {
    Forward(Vec<u8>),
    Interrupt,
    Ignore,
}

fn translate_key_event(key: KeyEvent) -> KeyAction {
    if matches!(key.kind, KeyEventKind::Release) {
        return KeyAction::Ignore;
    }
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => KeyAction::Interrupt,
        KeyCode::Char(ch) => translate_char_key(ch, key.modifiers),
        KeyCode::Enter => KeyAction::Forward(vec![b'\r']),
        KeyCode::Tab => KeyAction::Forward(vec![b'\t']),
        KeyCode::BackTab => KeyAction::Forward(b"\x1b[Z".to_vec()),
        KeyCode::Backspace => KeyAction::Forward(vec![0x7f]),
        KeyCode::Esc => KeyAction::Forward(vec![0x1b]),
        KeyCode::Left => KeyAction::Forward(b"\x1b[D".to_vec()),
        KeyCode::Right => KeyAction::Forward(b"\x1b[C".to_vec()),
        KeyCode::Up => KeyAction::Forward(b"\x1b[A".to_vec()),
        KeyCode::Down => KeyAction::Forward(b"\x1b[B".to_vec()),
        KeyCode::Home => KeyAction::Forward(b"\x1b[H".to_vec()),
        KeyCode::End => KeyAction::Forward(b"\x1b[F".to_vec()),
        KeyCode::PageUp => KeyAction::Forward(b"\x1b[5~".to_vec()),
        KeyCode::PageDown => KeyAction::Forward(b"\x1b[6~".to_vec()),
        KeyCode::Delete => KeyAction::Forward(b"\x1b[3~".to_vec()),
        KeyCode::Insert => KeyAction::Forward(b"\x1b[2~".to_vec()),
        _ => KeyAction::Ignore,
    }
}

fn translate_char_key(ch: char, modifiers: KeyModifiers) -> KeyAction {
    let alt = modifiers.contains(KeyModifiers::ALT);
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    if ctrl {
        if let Some(byte) = ctrl_char_to_byte(ch) {
            return if alt {
                KeyAction::Forward(vec![0x1b, byte])
            } else {
                KeyAction::Forward(vec![byte])
            };
        }
    }

    let mut bytes = Vec::new();
    if alt {
        bytes.push(0x1b);
    }
    let mut buffer = [0u8; 4];
    bytes.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
    KeyAction::Forward(bytes)
}

fn ctrl_char_to_byte(ch: char) -> Option<u8> {
    match ch {
        '@' | ' ' => Some(0x00),
        'a'..='z' => Some((ch as u8 - b'a') + 1),
        'A'..='Z' => Some((ch as u8 - b'A') + 1),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        _ => None,
    }
}
