use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};

use super::attach_input::{InputPoll, RawInput, RemoteInputFilter};
use super::client::{
    ensure_daemon, request_session_interrupt, send_daemon_request, send_worker_message,
    shutdown_worker_connection, write_worker_message,
};
use super::process_utils::identity_is_alive;
use super::sessions::resolve_session_id;
use super::types::{
    unix_millis_now, BackgroundPromptDecision, CtrlCProfile, DaemonRequest, DaemonResponse,
    LocalAttachResult, LocalInterruptProfile, RawTerminalGuard, SessionKind, SessionSnapshot,
    WorkerClientMessage, WorkerServerMessage, BACKGROUND_PROMPT_TIMEOUT,
};
use super::wire_prost::{daemon_wire_format_from_env, decode_worker_server_line, DaemonWireFormat};
use crate::console_title::OscTitleStripper;
use crate::ctrl_c_track::CtrlEventKind;
use crate::session::{
    InteractiveHooks, KeyboardEnhancementGuard, KeyboardEnhancementTracker, PtyInputSink,
};
use crate::voice::VoiceMode;

const INTERRUPT_EXIT_GRACE: Duration = Duration::from_millis(500);

/// Issue #517: log the captured [`crate::ctrl_c_track::CtrlEventKind`]
/// reason (Ctrl+C, SIGTERM, SIGHUP, SIGQUIT, window-close, ...) at the
/// moment `attach_to_session`'s interrupt-decision logic observes it, so
/// the new signal/console-event plumbing added by #517 is visible in real
/// runs. `site` identifies which of the interrupt-consulting call sites
/// (`daemon::commands`' `logs_follow` / `api_logs_follow`, and — via
/// [`log_interrupt_reason`] — `attach_to_session` and
/// `prompt_continue_in_background_terminal`) triggered the log, without
/// logging on every poll-loop tick: only the points where an interrupt
/// actually changes behavior.
///
/// This helper itself is still logging-only, but the follow-up it
/// anticipated has landed: the skip-the-prompt decision now lives in
/// [`background_decision_for_reason`] (#1208), which
/// `attach_to_session` consults before showing the interactive
/// background prompt, and in [`mid_prompt_decision_for_reason`], which
/// `prompt_continue_in_background_terminal` consults when a second
/// interrupt lands mid-prompt. Those two sites read the reason
/// themselves and log it through [`log_interrupt_reason`] so the logged
/// value and the decided value are the same read.
pub(super) fn log_observed_interrupt_reason(site: &str) {
    log_interrupt_reason(site, crate::ctrl_c_track::observed_event_kind());
}

/// Same log line, but for a reason the caller has *already* read out of
/// [`crate::ctrl_c_track`]. The two decision sites read the reason once
/// and pass it to both this logger and
/// [`background_decision_for_reason`], so the logged reason is always
/// the exact value the decision was made from. Re-reading the
/// process-global atomic per use could otherwise log one reason and act
/// on another if a second signal landed in between.
fn log_interrupt_reason(site: &str, reason: Option<CtrlEventKind>) {
    match reason {
        Some(kind) => {
            crate::verbose_log::log(format!("[clud] interrupt reason ({site}): {kind:?}"))
        }
        None => crate::verbose_log::log(format!(
            "[clud] interrupt reason ({site}): unknown (no probe fired)"
        )),
    }
}

/// Issue #1208: decides whether an observed interrupt [`CtrlEventKind`]
/// should skip the interactive "continue in the background?" prompt and
/// background the session directly.
///
/// `Some(ContinueInBackground)` covers the supervisor/terminal-loss
/// reasons (`CtrlClose`, `CtrlLogoff`, `CtrlShutdown`, `Hup`, `Term`).
/// What unifies them is that nobody is left at a terminal who could
/// answer the prompt: the console window is closing, the controlling
/// terminal (or its session leader) is already gone, or a supervisor is
/// tearing the process down. The prompt would burn its own 5-second
/// timeout and then background anyway, so we skip straight to the
/// answer. Windows makes the cost concrete — `CTRL_CLOSE_EVENT` and
/// friends give the handler only ~5 seconds before the OS kills the
/// process, which the prompt's timeout would consume entirely. And
/// backgrounding is the non-destructive choice regardless: it matches
/// what the prompt's own timeout and the non-interactive path
/// (`prompt_continue_in_background_noninteractive`) already do.
///
/// `None` means "no opinion — keep today's interactive prompt".
/// `Quit` (SIGQUIT) deliberately keeps the prompt for now: treating it
/// as an `EndSession` reason is arguable and can be decided later from
/// logged data. `Unknown` and a missing reason (`None` input) also keep
/// the prompt, so an unmapped future event never silently changes
/// behavior.
///
/// The match is exhaustive with no wildcard arm on purpose: adding a new
/// `CtrlEventKind` variant without updating this function is a compile
/// error here, not a silent "keep prompting" default.
fn background_decision_for_reason(kind: Option<CtrlEventKind>) -> Option<BackgroundPromptDecision> {
    match kind? {
        CtrlEventKind::CtrlClose
        | CtrlEventKind::CtrlLogoff
        | CtrlEventKind::CtrlShutdown
        | CtrlEventKind::Hup
        | CtrlEventKind::Term => Some(BackgroundPromptDecision::ContinueInBackground),
        CtrlEventKind::CtrlC
        | CtrlEventKind::CtrlBreak
        | CtrlEventKind::Quit
        | CtrlEventKind::Unknown => None,
    }
}

/// Issue #1208: the mid-prompt variant of
/// [`background_decision_for_reason`], used when a second interrupt
/// lands while the countdown prompt is already on screen.
///
/// There is no "no opinion" case here: the user is being asked a
/// question and the interrupt *is* an answer. A terminal-loss reason
/// backgrounds; everything else — a second Ctrl+C, Ctrl+Break, SIGQUIT,
/// an unmapped future event, or no stamped reason at all — keeps the
/// pre-#1208 behavior of ending the session. Split out from
/// `prompt_continue_in_background_terminal` (which needs a real
/// terminal) so the fallback is table-testable and a future edit cannot
/// silently flip the default.
fn mid_prompt_decision_for_reason(kind: Option<CtrlEventKind>) -> BackgroundPromptDecision {
    background_decision_for_reason(kind).unwrap_or(BackgroundPromptDecision::EndSession)
}

/// Announce a background prompt that [`background_decision_for_reason`]
/// skipped (#1208), naming both the observed reason and the decision it
/// produced. One line, not two: [`crate::verbose_log::log`] already
/// writes to stderr as well as the log file, so a user who *is* still
/// watching the terminal sees why no prompt appeared without a second
/// near-identical `eprintln!`.
fn announce_skipped_background_prompt(
    reason: Option<CtrlEventKind>,
    decision: BackgroundPromptDecision,
) {
    let reason = match reason {
        Some(kind) => format!("{kind:?}"),
        None => "unknown".to_string(),
    };
    crate::verbose_log::log(format!(
        "[clud] interrupt reason {reason} leaves nobody at the terminal; \
         skipping the background prompt ({decision:?})"
    ));
}

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
        | DaemonResponse::ApiSessionKilled { .. }
        | DaemonResponse::Interrupted { .. }
        | DaemonResponse::AdoptKillAck { .. }
        | DaemonResponse::Gc { .. }
        | DaemonResponse::LiveCwds { .. }
        | DaemonResponse::ShutdownAck { .. }
        | DaemonResponse::ReapOrphansAck { .. }
        | DaemonResponse::Metrics { .. }
        | DaemonResponse::ProcSnapshot { .. }
        | DaemonResponse::ClientLeaseAcquired { .. }
        | DaemonResponse::ClientLeaseReleased { .. } => 1,
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
                if !identity_is_alive(&session.worker_identity()) {
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

    let interactive = matches!(session.kind, SessionKind::Pty)
        && io::stdin().is_terminal()
        && io::stdout().is_terminal();
    // #1363: the same kitty keyboard frame the local PTY pump holds
    // (`session::RawTerminalGuard`). Pushed before the reader starts, so the
    // push cannot land inside a relayed escape sequence, and bound here
    // rather than in `run_remote_interactive` so it outlives `reader.join()`
    // below: the child's frames are unwound only after its last output has
    // been observed, on every return path and on unwind.
    let keyboard = interactive.then(KeyboardEnhancementGuard::push);
    let child_keyboard = keyboard
        .as_ref()
        .map(KeyboardEnhancementGuard::child_tracker);
    // #1372: one stripper for the whole attach, so a title split across
    // two `Output` messages is still dropped.
    let mut osc_strip = OscTitleStripper::new();
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
                            let mut stdout = io::stdout().lock();
                            relay_worker_output(
                                &mut stdout,
                                &bytes,
                                child_keyboard.as_deref(),
                                &mut osc_strip,
                            );
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

    let local_result = if interactive {
        run_remote_interactive(Arc::clone(&writer), format, interrupted, session.detachable)
    } else {
        wait_for_remote_or_interrupt(&exit_code, interrupted)
    };

    let (local_result, backgrounded) = match local_result {
        LocalAttachResult::Completed(code) => (code, false),
        LocalAttachResult::InterruptRequested(interrupt) => {
            // Read the reason once and reuse it for both the log line and
            // the #1208 decision, so the two can never disagree.
            let reason = crate::ctrl_c_track::observed_event_kind();
            log_interrupt_reason("attach_to_session", reason);
            interrupted.store(false, Ordering::SeqCst);
            if session.detachable {
                // Issue #1208: a supervisor/terminal-loss reason (window
                // close, logoff, shutdown, SIGHUP, SIGTERM) means there
                // is no longer anyone at a terminal to answer the
                // prompt — and on Windows only ~5s before the OS kills
                // us — so background directly instead of prompting.
                // Rationale lives on `background_decision_for_reason`.
                let decision = match background_decision_for_reason(reason) {
                    Some(decision) => {
                        announce_skipped_background_prompt(reason, decision);
                        decision
                    }
                    None => prompt_continue_in_background(interrupted),
                };
                match decision {
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
    // The relay has stopped, so every child frame is counted: unwind them,
    // then the attach's own frame, before the shell gets the terminal back.
    drop(keyboard);
    if local_result == 130 {
        return 130;
    }
    let final_exit_code = exit_code
        .lock()
        .expect("exit code mutex poisoned")
        .unwrap_or(local_result);
    final_exit_code
}

/// Write one chunk of the session's output to the local terminal, as the
/// local pump's output reader does: the raw bytes go first to the attach's
/// keyboard-frame tracker (#1363), then the child's OSC 0/2 title writes are
/// stripped (#1372) so they cannot overwrite clud's stamped console title.
/// The worker keeps the raw bytes for its backlog, log and transcript.
fn relay_worker_output(
    out: &mut dyn Write,
    bytes: &[u8],
    child_keyboard: Option<&KeyboardEnhancementTracker>,
    osc_strip: &mut OscTitleStripper,
) {
    if let Some(tracker) = child_keyboard {
        tracker.observe(bytes);
    }
    let _ = out.write_all(&osc_strip.process(bytes));
    let _ = out.flush();
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

/// How long the attach loop waits for input before it ticks the voice hook,
/// drains drag-drop chunks and checks the terminal size.
const ATTACH_INPUT_TICK: Duration = Duration::from_millis(25);

/// How long a held partial bracketed-paste prefix (usually a lone Esc)
/// waits for its continuation before it is released to the worker.
const ATTACH_PENDING_FLUSH: Duration = Duration::from_millis(5);

fn run_remote_interactive(
    writer: Arc<Mutex<TcpStream>>,
    format: DaemonWireFormat,
    interrupted: &AtomicBool,
    _detachable: bool,
) -> LocalAttachResult {
    // #1355: raw bytes, not `crossterm::event`, so terminal replies such as
    // the answer to ConPTY's startup `ESC[6n` reach the worker. Same setup
    // order as `runner_execution.rs`: the input reader first (on Windows it
    // snapshots the original console mode), then VT input, then raw mode.
    // Locals drop in reverse, so raw mode is undone first.
    let mut input = RawInput::start();
    let _console_vt = crate::console_setup::enable_console_vt_input();
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
    let mut filter = RemoteInputFilter::new();
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
    let send_input = |bytes: &[u8], submit: bool| {
        let _ = send_worker_message(
            &writer,
            &WorkerClientMessage::Input {
                data_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
                submit,
            },
            format,
        );
    };
    // Without crossterm's event source nothing reports a resize, so the
    // loop compares the terminal size on every tick.
    let mut last_size = crossterm::terminal::size().ok();

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
        // Issue #517: this loop polls every 25ms, so the reason is
        // deliberately NOT logged here (would spam the log on every
        // tick). `attach_to_session`'s single `InterruptRequested` match
        // arm logs it once, covering both this path and
        // `wait_for_remote_or_interrupt`'s.
        if interrupted.load(Ordering::SeqCst) {
            return LocalAttachResult::InterruptRequested(LocalInterruptProfile::now());
        }
        let wait = if filter.has_pending() {
            ATTACH_PENDING_FLUSH
        } else {
            ATTACH_INPUT_TICK
        };
        match input.poll(wait) {
            InputPoll::Chunk(chunk) => {
                let filtered = filter.process(&chunk);
                if filtered.interrupt {
                    return LocalAttachResult::InterruptRequested(LocalInterruptProfile::now());
                }
                if !filtered.bytes.is_empty() {
                    send_input(&filtered.bytes, filtered.submit());
                }
                if voice.intercept_f3() {
                    for _ in 0..filtered.f3.presses {
                        if let Err(err) = voice.on_f3_press(&mut sink) {
                            eprintln!("[clud] warning: voice F3 press hook failed: {}", err);
                        }
                    }
                    for _ in 0..filtered.f3.releases {
                        if let Err(err) = voice.on_f3_release(&mut sink) {
                            eprintln!("[clud] warning: voice F3 release hook failed: {}", err);
                        }
                    }
                }
            }
            InputPoll::Idle => {
                let pending = filter.flush_pending();
                if !pending.is_empty() {
                    send_input(&pending, false);
                }
            }
            InputPoll::Closed => return LocalAttachResult::Completed(1),
        }
        let size = crossterm::terminal::size().ok();
        if size != last_size {
            last_size = size;
            if let Some((cols, rows)) = size {
                let _ = send_worker_message(
                    &writer,
                    &WorkerClientMessage::Resize { rows, cols },
                    format,
                );
            }
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
                send_input(&chunk, false);
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
            // Close the prompt line first, then log — the guard above
            // still holds raw mode, so anything written here lands
            // immediately after the last rendered countdown line.
            eprintln!();
            let reason = crate::ctrl_c_track::observed_event_kind();
            log_interrupt_reason("prompt_continue_in_background_terminal", reason);
            // Issue #1208: a second Ctrl+C while the prompt is up still
            // ends the session, but a SIGHUP/SIGTERM/window-close
            // arriving mid-prompt backgrounds instead of killing it.
            return mid_prompt_decision_for_reason(reason);
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

    /// #1363: a child TUI in a daemon session pushes a kitty keyboard frame,
    /// then the attach ends (detach, Ctrl+C, the session dying) before the
    /// child pops it. The relay must feed the child's output to the attach's
    /// keyboard guard, which unwinds that frame and then its own, leaving the
    /// terminal's pre-attach frames alone.
    #[test]
    fn attach_unwinds_a_relayed_child_keyboard_frame_before_its_own() {
        let mut keyboard = KeyboardEnhancementGuard::with_pushed(true);
        let tracker = keyboard.child_tracker();
        let mut terminal = Vec::new();
        let mut osc_strip = OscTitleStripper::new();
        relay_worker_output(&mut terminal, b"tui\x1b[>", Some(&tracker), &mut osc_strip);
        relay_worker_output(&mut terminal, b"7u frame", Some(&tracker), &mut osc_strip);

        keyboard.unwind_to(&mut terminal);

        assert_eq!(
            terminal, b"tui\x1b[>7u frame\x1b[<1u\x1b[<1u",
            "child frame, then the attach's own frame, pop in LIFO order"
        );
    }

    /// A child that balances its own frame leaves only the attach's frame
    /// to pop, and the relayed bytes reach the terminal unchanged.
    #[test]
    fn attach_pops_only_its_own_frame_after_a_balanced_child() {
        let mut keyboard = KeyboardEnhancementGuard::with_pushed(true);
        let tracker = keyboard.child_tracker();
        let mut terminal = Vec::new();
        let mut osc_strip = OscTitleStripper::new();
        relay_worker_output(
            &mut terminal,
            b"\x1b[>1uhi\x1b[<u",
            Some(&tracker),
            &mut osc_strip,
        );

        keyboard.unwind_to(&mut terminal);
        keyboard.unwind_to(&mut terminal);

        assert_eq!(
            terminal, b"\x1b[>1uhi\x1b[<u\x1b[<1u",
            "unwind is idempotent"
        );
    }

    /// #1372: the local PTY pump strips the child's OSC 0/2 title writes so
    /// they cannot overwrite clud's stamped console title; the attach relay
    /// must do the same, including a title split across two output chunks,
    /// while other output and the keyboard-frame tracking pass through.
    #[test]
    fn attach_relay_strips_child_osc_titles_like_the_local_pump() {
        let mut keyboard = KeyboardEnhancementGuard::with_pushed(true);
        let tracker = keyboard.child_tracker();
        let mut terminal = Vec::new();
        let mut osc_strip = OscTitleStripper::new();
        relay_worker_output(
            &mut terminal,
            b"a\x1b]0;bel-title\x07b\x1b[>1u\x1b]2;st-ti",
            Some(&tracker),
            &mut osc_strip,
        );
        relay_worker_output(
            &mut terminal,
            b"tle\x1b\\c\x1b]8;;u\x07",
            Some(&tracker),
            &mut osc_strip,
        );

        assert_eq!(
            terminal, b"ab\x1b[>1uc\x1b]8;;u\x07",
            "OSC 0 and a split OSC 2 are dropped; CSI and OSC 8 pass through"
        );

        terminal.clear();
        keyboard.unwind_to(&mut terminal);
        assert_eq!(
            terminal, b"\x1b[<1u\x1b[<1u",
            "the child's relayed frame was still tracked"
        );
    }

    #[test]
    fn noninteractive_background_prompt_always_backgrounds() {
        assert_eq!(
            prompt_continue_in_background_noninteractive(),
            BackgroundPromptDecision::ContinueInBackground
        );
    }

    /// Issue #1208: table-tests every [`CtrlEventKind`] variant against
    /// [`background_decision_for_reason`]. The function's `match` is
    /// exhaustive with no wildcard arm, so a newly added `CtrlEventKind`
    /// variant that isn't added to this table (and to the function
    /// itself) is a compile error rather than a silent "keep prompting"
    /// default.
    #[test]
    fn background_decision_for_reason_maps_every_ctrl_event_kind() {
        let continue_in_background = Some(BackgroundPromptDecision::ContinueInBackground);
        let table: [(CtrlEventKind, Option<BackgroundPromptDecision>); 9] = [
            (CtrlEventKind::CtrlClose, continue_in_background),
            (CtrlEventKind::CtrlLogoff, continue_in_background),
            (CtrlEventKind::CtrlShutdown, continue_in_background),
            (CtrlEventKind::Hup, continue_in_background),
            (CtrlEventKind::Term, continue_in_background),
            (CtrlEventKind::CtrlC, None),
            (CtrlEventKind::CtrlBreak, None),
            (CtrlEventKind::Quit, None),
            (CtrlEventKind::Unknown, None),
        ];
        for (kind, expected) in table {
            let actual = background_decision_for_reason(Some(kind));
            assert_eq!(actual, expected, "{kind:?}");
        }
    }

    #[test]
    fn background_decision_for_reason_keeps_the_prompt_when_no_reason_was_stamped() {
        assert_eq!(background_decision_for_reason(None), None);
    }

    /// Issue #1208: the mid-prompt fallback has no "no opinion" case, so
    /// this table covers every [`CtrlEventKind`] variant *plus* the
    /// no-reason-stamped input. Everything that is not a terminal-loss
    /// reason must keep the pre-#1208 behavior of ending the session —
    /// the dangerous direction would be silently backgrounding on a
    /// second Ctrl+C.
    #[test]
    fn mid_prompt_decision_backgrounds_only_on_terminal_loss() {
        let bg = BackgroundPromptDecision::ContinueInBackground;
        let end = BackgroundPromptDecision::EndSession;
        let table: [(Option<CtrlEventKind>, BackgroundPromptDecision); 10] = [
            (Some(CtrlEventKind::CtrlClose), bg),
            (Some(CtrlEventKind::CtrlLogoff), bg),
            (Some(CtrlEventKind::CtrlShutdown), bg),
            (Some(CtrlEventKind::Hup), bg),
            (Some(CtrlEventKind::Term), bg),
            (Some(CtrlEventKind::CtrlC), end),
            (Some(CtrlEventKind::CtrlBreak), end),
            (Some(CtrlEventKind::Quit), end),
            (Some(CtrlEventKind::Unknown), end),
            (None, end),
        ];
        for (reason, expected) in table {
            let actual = mid_prompt_decision_for_reason(reason);
            assert_eq!(actual, expected, "{reason:?}");
        }
    }
}
