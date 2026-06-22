use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};

use super::client::{
    ensure_daemon, request_session_interrupt, send_daemon_request, send_worker_message,
    shutdown_worker_connection, write_worker_message,
};
use super::keys::translate_key_event;
use super::process_utils::pid_is_alive;
use super::sessions::resolve_session_id;
use super::types::{
    unix_millis_now, BackgroundPromptDecision, CtrlCProfile, DaemonRequest, DaemonResponse,
    KeyAction, LocalAttachResult, LocalInterruptProfile, RawTerminalGuard, SessionKind,
    SessionSnapshot, WorkerClientMessage, WorkerServerMessage, BACKGROUND_PROMPT_TIMEOUT,
};
use super::wire_prost::{daemon_wire_format_from_env, decode_worker_server_line, DaemonWireFormat};
use crate::session::{InteractiveHooks, PtyInputSink};
use crate::voice::VoiceMode;

const INTERRUPT_EXIT_GRACE: Duration = Duration::from_millis(500);

/// `PtyInputSink` impl that forwards bytes to the daemon-owned PTY as a
/// `WorkerClientMessage::Input` TCP frame. This is what lets centralized
/// mode wire `VoiceMode` (and other `InteractiveHooks` impls) without
/// having a local `NativePtyProcess` to write to. Synthetic input from
/// voice transcripts, drag-drop paths, etc. lands at the daemon worker
/// and is forwarded to the PTY master alongside real keystrokes.
struct WorkerInputSink {
    writer: Arc<Mutex<TcpStream>>,
    format: DaemonWireFormat,
}

impl PtyInputSink for WorkerInputSink {
    fn write_input(&mut self, bytes: &[u8], submit: bool) -> io::Result<()> {
        let msg = WorkerClientMessage::Input {
            data_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
            submit,
        };
        send_worker_message(&self.writer, &msg, self.format)
    }
}

pub(super) fn run_attach(session_id: &str, state_dir: &Path, interrupted: &AtomicBool) -> i32 {
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
        DaemonResponse::Session { session } => {
            if !session.attachable {
                eprintln!(
                    "[clud] session {} is a repeat job and cannot be attached",
                    session.id
                );
                return 1;
            }
            attach_to_session(state_dir, &session, interrupted)
        }
        DaemonResponse::Error { message } => {
            eprintln!("[clud] daemon error: {}", message);
            1
        }
        DaemonResponse::Created { .. }
        | DaemonResponse::Terminated { .. }
        | DaemonResponse::Interrupted { .. }
        | DaemonResponse::AdoptKillAck { .. }
        | DaemonResponse::Gc { .. }
        | DaemonResponse::LiveCwds { .. }
        | DaemonResponse::ShutdownAck { .. }
        | DaemonResponse::ReapOrphansAck { .. }
        | DaemonResponse::Metrics { .. } => 1,
    }
}

pub(super) fn attach_to_session(
    state_dir: &Path,
    session: &SessionSnapshot,
    interrupted: &AtomicBool,
) -> i32 {
    let format = match daemon_wire_format_from_env() {
        Ok(format) => format,
        Err(err) => {
            eprintln!("[clud] {err}");
            return 1;
        }
    };
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
        let terminal =
            matches!(session.kind, SessionKind::Pty).then(crate::graphics::detect_current_terminal);
        let (rows, cols) = super::io_helpers::terminal_dimensions();
        if let Err(err) = write_worker_message(
            &mut stream,
            &WorkerClientMessage::Attach {
                terminal,
                rows: Some(rows),
                cols: Some(cols),
            },
            format,
        ) {
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
                // EOF before any handshake response. Two known causes:
                //   1. Worker really is gone (process died between connect
                //      and our read).
                //   2. Transient — worker's `handle_worker_client` returned
                //      early without writing, e.g. it read 0 bytes back from
                //      our handshake because our `write_all` and the
                //      worker's `read_line` raced under TCP buffering quirks.
                // Within the retry window, give the worker another shot: the
                // second attempt usually slots in cleanly. Outside the window,
                // surface the EOF as a real failure.
                if started.elapsed() < attach_retry_window {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
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

        let message = match decode_worker_server_line(&line) {
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
                let Ok(message) = decode_worker_server_line(&line) else {
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
        run_remote_interactive(Arc::clone(&writer), format, interrupted, session.detachable)
    } else {
        wait_for_remote_or_interrupt(&exit_code, interrupted)
    };

    let (local_result, backgrounded) = match local_result {
        LocalAttachResult::Completed(code) => (code, false),
        LocalAttachResult::InterruptRequested(interrupt) => {
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
                        send_interrupt_fast_path(
                            state_dir,
                            &session.id,
                            &writer,
                            format,
                            interrupt,
                        );
                        (130, false)
                    }
                }
            } else {
                send_interrupt_fast_path(state_dir, &session.id, &writer, format, interrupt);
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

fn send_interrupt_fast_path(
    state_dir: &Path,
    session_id: &str,
    writer: &Arc<Mutex<TcpStream>>,
    format: DaemonWireFormat,
    interrupt: LocalInterruptProfile,
) {
    let now = unix_millis_now();
    let profile = CtrlCProfile {
        cli_pid: Some(std::process::id()),
        cli_observed_at_ms: Some(interrupt.observed_at_ms),
        cli_handoff_at_ms: Some(now),
        cli_return_ready_at_ms: Some(now),
        cli_handoff_ms: Some(interrupt.elapsed_ms()),
        fast_path: true,
        ..CtrlCProfile::default()
    };
    if let Err(err) = request_session_interrupt(state_dir, session_id, profile.clone()) {
        eprintln!("[clud] warning: failed to hand Ctrl+C to daemon: {err}");
        if let Err(err) = send_worker_message(
            writer,
            &WorkerClientMessage::Interrupt {
                profile: Some(profile),
            },
            format,
        ) {
            eprintln!("[clud] warning: failed to hand Ctrl+C to daemon worker: {err}");
        }
    }
    let _ = shutdown_worker_connection(writer);
}

fn run_remote_interactive(
    writer: Arc<Mutex<TcpStream>>,
    format: DaemonWireFormat,
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
    // VoiceMode + PtyInputSink: same `InteractiveHooks` plumbing the
    // local-PTY pump uses, just with input bytes routed through the
    // daemon-worker TCP socket instead of `NativePtyProcess::write_impl`.
    // When voice is disabled by env (`CLUD_VOICE_*` unset, no model
    // present) `intercept_f3()` returns false and all the hook calls
    // below are constant-time no-ops.
    let mut voice = VoiceMode::from_env();
    let mut sink = WorkerInputSink {
        writer: Arc::clone(&writer),
        format,
    };

    // Issue #79: register the console IDropTarget so dropped paths reach
    // the daemon-owned PTY just like keystrokes. Held for the lifetime of
    // the interactive attach; the worker displacement thread refreshes
    // the registration as needed. No-op on POSIX.
    #[cfg(windows)]
    let (_dnd_guard, dnd_rx) = crate::startup::try_register_console_drop_target_pty();
    #[cfg(not(windows))]
    let (_dnd_guard, dnd_rx): (Option<()>, Option<std::sync::mpsc::Receiver<Vec<u8>>>) =
        (None, None);
    loop {
        if interrupted.load(Ordering::SeqCst) {
            return LocalAttachResult::InterruptRequested(LocalInterruptProfile::now());
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
                            format,
                        );
                    }
                    KeyAction::Interrupt => {
                        return LocalAttachResult::InterruptRequested(LocalInterruptProfile::now());
                    }
                    KeyAction::F3Press => {
                        if voice.intercept_f3() {
                            if let Err(err) = voice.on_f3_press(&mut sink) {
                                eprintln!("[clud] warning: voice F3 press hook failed: {}", err);
                            }
                        }
                    }
                    KeyAction::F3Release => {
                        if voice.intercept_f3() {
                            if let Err(err) = voice.on_f3_release(&mut sink) {
                                eprintln!("[clud] warning: voice F3 release hook failed: {}", err);
                            }
                        }
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
                        format,
                    );
                }
                Ok(Event::Resize(cols, rows)) => {
                    let _ = send_worker_message(
                        &writer,
                        &WorkerClientMessage::Resize { rows, cols },
                        format,
                    );
                }
                Ok(_) => {}
                Err(_) => return LocalAttachResult::Completed(1),
            },
            Ok(false) => {}
            Err(_) => return LocalAttachResult::Completed(1),
        }
        // Tick the voice hook even when no keyboard event arrived: this
        // drains pending whisper transcripts into `WorkerInputSink` and
        // runs the VAD auto-stop for terminals that don't emit F3
        // release events.
        if let Err(err) = voice.on_tick(&mut sink) {
            eprintln!("[clud] warning: voice tick hook failed: {}", err);
        }

        // Drain any drop-target bytes the OLE worker pushed since the
        // last tick. Each chunk is one dropped path (or a paste-batched
        // group). `submit=false` keeps the cursor in the input box so
        // the user can edit before submitting, matching the local-PTY
        // runner's behavior.
        if let Some(rx) = &dnd_rx {
            while let Ok(chunk) = rx.try_recv() {
                let _ = send_worker_message(
                    &writer,
                    &WorkerClientMessage::Input {
                        data_b64: base64::engine::general_purpose::STANDARD.encode(&chunk),
                        submit: false,
                    },
                    format,
                );
            }
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
        LocalAttachResult::InterruptRequested(LocalInterruptProfile::now())
    } else if exit_code
        .lock()
        .expect("exit code mutex poisoned")
        .is_some()
    {
        wait_for_late_interrupt(interrupted)
    } else {
        LocalAttachResult::Completed(0)
    }
}

fn wait_for_late_interrupt(interrupted: &AtomicBool) -> LocalAttachResult {
    let started = Instant::now();
    while started.elapsed() < INTERRUPT_EXIT_GRACE {
        if interrupted.load(Ordering::SeqCst) {
            return LocalAttachResult::InterruptRequested(LocalInterruptProfile::now());
        }
        thread::sleep(Duration::from_millis(10));
    }
    LocalAttachResult::Completed(0)
}

fn prompt_continue_in_background(interrupted: &AtomicBool) -> BackgroundPromptDecision {
    if io::stdin().is_terminal() && io::stderr().is_terminal() {
        prompt_continue_in_background_terminal(interrupted)
    } else {
        prompt_continue_in_background_noninteractive()
    }
}

fn prompt_continue_in_background_terminal(interrupted: &AtomicBool) -> BackgroundPromptDecision {
    let _guard = match RawTerminalGuard::enter() {
        Ok(guard) => guard,
        Err(_) => return BackgroundPromptDecision::ContinueInBackground,
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
            return BackgroundPromptDecision::ContinueInBackground;
        }
        if interrupted.swap(false, Ordering::SeqCst) {
            eprintln!();
            return BackgroundPromptDecision::EndSession;
        }
        match event::poll(Duration::from_millis(100)) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) => match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        eprintln!();
                        return BackgroundPromptDecision::ContinueInBackground;
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        eprintln!();
                        return BackgroundPromptDecision::EndSession;
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        eprintln!();
                        return BackgroundPromptDecision::EndSession;
                    }
                    _ => {}
                },
                Ok(_) => {}
                Err(_) => {
                    eprintln!();
                    return BackgroundPromptDecision::ContinueInBackground;
                }
            },
            Ok(false) => {}
            Err(_) => {
                eprintln!();
                return BackgroundPromptDecision::ContinueInBackground;
            }
        }
    }
}

pub(super) fn prompt_continue_in_background_noninteractive() -> BackgroundPromptDecision {
    eprintln!("[clud] non-interactive attach interrupted; session continues in the background");
    BackgroundPromptDecision::ContinueInBackground
}

fn render_background_prompt(remaining: u64) {
    eprintln!(
        "[clud] continue session in the background? [Y/n] auto-backgrounding in {}s",
        remaining
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noninteractive_background_prompt_always_backgrounds() {
        assert_eq!(
            prompt_continue_in_background_noninteractive(),
            BackgroundPromptDecision::ContinueInBackground
        );
    }
}
