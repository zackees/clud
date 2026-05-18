//! Subprocess- and PTY-mode runners for a single [`LaunchPlan`].
//!
//! These were inlined in `main.rs` until the file crossed 1k LOC. They
//! contain the per-iteration loop, the stream-json fallback, the
//! Ctrl-C-aware child teardown, and the launch-mode-specific wiring for
//! the OLE drag-drop registration.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::command;
use crate::console_setup::enable_console_vt_input;
use crate::loop_artifacts;
use crate::loop_check::{
    check_loop_markers, check_loop_markers_with_output, loop_unconverged_exit,
};
use crate::process_tree;
use crate::session;
use crate::stream_json;
use crate::subprocess;
use crate::voice;
use crate::win_creation_flags;

/// Build the child environment: inherit parent env + inject tracking vars.
/// Deduplicates keys so we never pass the same var twice.
pub fn child_env() -> Vec<(String, String)> {
    let originator_key = running_process_core::ORIGINATOR_ENV_VAR;

    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k != "IN_CLUD" && k != originator_key)
        .collect();

    env.push(("IN_CLUD".to_string(), "1".to_string()));

    let originator_value = format!("CLUD:{}", std::process::id());
    env.push((originator_key.to_string(), originator_value));

    env
}

pub fn get_terminal_size() -> (u16, u16) {
    let probe = terminal_size::terminal_size().map(|(w, h)| (w.0, h.0));
    resolve_terminal_size(probe)
}

/// Translate a `(cols, rows)` probe result into a `(rows, cols)` size to hand
/// to the PTY. `None` means no real terminal — return a safe fallback.
/// 200 cols is wide enough that typical codex/claude output doesn't wrap
/// awkwardly, but stays within the range real terminal emulators actually
/// exercise — passing 32767 to ConPTY pushes layout math into corners that
/// trigger cursor drift in ratatui/Ink-based TUIs (issue #31, T3).
pub fn resolve_terminal_size(probe: Option<(u16, u16)>) -> (u16, u16) {
    match probe {
        Some((cols, rows)) => (rows, cols),
        None => (24, 200),
    }
}

pub fn normalize_exit_code(code: i32) -> i32 {
    match code {
        -2 => 130,
        -9 => 137,
        -15 => 143,
        _ => code,
    }
}

/// Translate the final loop exit code into a `(summary, error)` pair
/// for `LoopSession::on_loop_end`. The mapping mirrors
/// `check_loop_markers`/`loop_unconverged_exit`:
///   - 0 → DONE
///   - 2 → iteration cap exhausted
///   - 3 → BLOCKED marker
///   - 130 → interrupt (Ctrl-C)
///   - anything else → "exit code N" + same as the error string
pub fn summarize_loop_outcome(exit_code: i32) -> (&'static str, Option<String>) {
    match exit_code {
        0 => ("DONE", None),
        2 => (
            "iteration cap exhausted",
            Some("iteration cap exhausted".to_string()),
        ),
        3 => ("BLOCKED", Some("blocked by agent".to_string())),
        130 => ("interrupted", Some("Interrupted by user".to_string())),
        _ => ("exit", Some(format!("exit code {exit_code}"))),
    }
}

pub fn run_plan_subprocess(
    plan: &command::LaunchPlan,
    verbose: bool,
    interrupted: &AtomicBool,
    mut loop_session: Option<&mut loop_artifacts::LoopSession>,
) -> i32 {
    use std::path::PathBuf;

    use running_process_core::{Containment, NativeProcess, ProcessConfig, StderrMode, StdinMode};

    let env = child_env();
    let mut last_exit = 0i32;

    for iteration in 0..plan.iterations {
        // Re-check the interrupted flag at the top of every iteration. A
        // Ctrl+C that fires between the previous child's reap and our next
        // spawn would otherwise be silently swallowed and we'd cheerfully
        // launch another codex run. 130 is the conventional SIGINT exit
        // code and mirrors what `ProcessOutcome::Interrupted` produces.
        if interrupted.load(Ordering::SeqCst) {
            eprintln!("[clud] interrupted via Ctrl+C");
            return 130;
        }

        let iter_num = iteration + 1;
        if plan.iterations > 1 {
            eprintln!("[clud] iteration {}/{}", iter_num, plan.iterations);
        }
        if let Some(s) = loop_session.as_deref_mut() {
            s.on_iteration_start(iter_num);
        }

        if verbose {
            eprintln!("[clud] exec (subprocess): {}", plan.command.join(" "));
        }

        let config = ProcessConfig {
            command: subprocess::command_spec_for_subprocess(plan.command.clone()),
            cwd: plan.cwd.as_ref().map(PathBuf::from),
            env: Some(env.clone()),
            // When stream-json progress is on, we capture stdout so we can
            // drain it line-by-line and route each event through the
            // renderer. Otherwise stdio is inherited and the child writes
            // directly to our console (preserving any TUI behavior).
            capture: plan.stream_json_progress,
            stderr_mode: StderrMode::Stdout,
            // Windows: spawn the backend in its own console process
            // group so the OS does not deliver `CTRL_C_EVENT` to the
            // child (or its descendants) when the user hits Ctrl+C.
            // clud's own `ctrlc` handler catches the event and tears
            // the child tree down via `process_tree::kill_tree`; the
            // child only receives a hard termination, never a stray
            // signal that would let the `nodejs-wheel` Python launcher
            // raise `KeyboardInterrupt` and dump a traceback over
            // clud's clean exit message. POSIX has no equivalent flag
            // and the terminal foreground-process-group behavior is
            // already correct, so the helper returns `None` there.
            creationflags: win_creation_flags::user_facing_backend_creationflags(),
            create_process_group: false,
            stdin_mode: StdinMode::Inherit,
            nice: None,
            // Issue #9: Claude/Codex spawn tool subprocesses (cargo test,
            // npm test, long builds) that leak as zombies when a clud
            // session dies abnormally (crash, terminal close, Task Manager
            // kill). `Containment::Contained` binds the child tree's
            // lifetime to ours: PR_SET_PDEATHSIG(SIGKILL) on Linux, a
            // kill-on-close Job Object on Windows. The daemon path already
            // sets this (daemon.rs); direct subprocess runs now do too.
            containment: Some(Containment::Contained),
        };

        let process = NativeProcess::new(config);
        if let Err(e) = process.start() {
            eprintln!("[clud] failed to execute {}: {}", plan.command[0], e);
            if let Some(s) = loop_session.as_deref_mut() {
                s.on_iteration_end(iter_num, 1, Some(format!("failed to start: {e}")));
            }
            return 1;
        }

        // Issue #95: in stream-json mode we also accumulate the rendered
        // output so we can fall back to scanning for the
        // `<<<CLUD_LOOP_DONE: ...>>>` token if the agent skipped the
        // marker file. In inherited-stdio mode the child writes directly
        // to the user's terminal and we never see the bytes — the token
        // fallback is unavailable there.
        let mut captured_output = String::new();
        let exit_code = if plan.stream_json_progress {
            run_with_stream_json_renderer(&process, interrupted, &mut captured_output)
        } else {
            run_with_inherited_stdio(&process, interrupted)
        };
        match exit_code {
            ProcessOutcome::Exited(code) => {
                last_exit = code;
                if let Some(s) = loop_session.as_deref_mut() {
                    s.on_iteration_end(iter_num, code, None);
                }
                if last_exit != 0 && plan.iterations > 1 {
                    eprintln!(
                        "[clud] iteration {} failed with exit code {}",
                        iter_num, last_exit
                    );
                    return last_exit;
                }
            }
            ProcessOutcome::Interrupted => {
                eprintln!("[clud] interrupted via Ctrl+C");
                if let Some(s) = loop_session.as_deref_mut() {
                    s.on_iteration_end(iter_num, 130, Some("Interrupted by user".to_string()));
                }
                return 130;
            }
            ProcessOutcome::Error => {
                if let Some(s) = loop_session.as_deref_mut() {
                    s.on_iteration_end(iter_num, 1, Some("runner error".to_string()));
                }
                return 1;
            }
        }

        if let Some(code) = check_loop_markers_with_output(plan, iter_num, &captured_output) {
            return code;
        }
    }

    if let Some(code) = loop_unconverged_exit(plan) {
        return code;
    }

    last_exit
}

/// Outcome of one subprocess-mode iteration. Threaded through both the
/// inherited-stdio path and the stream-json renderer path so the outer loop
/// in `run_plan_subprocess` can stay uniform.
enum ProcessOutcome {
    Exited(i32),
    Interrupted,
    Error,
}

/// Tear down a backend child that has not exited yet because the user
/// just hit Ctrl+C.
///
/// Sequence:
///
/// 1. Windows-only: send `CTRL_BREAK_EVENT` to the child's console
///    process group. The backend is spawned with
///    `CREATE_NEW_PROCESS_GROUP` so the OS does *not* deliver the
///    user's `CTRL_C_EVENT` to it — that prevents stray
///    `KeyboardInterrupt` tracebacks from blocking `subprocess` waits
///    in the `nodejs-wheel` Python launcher used by some Claude Code
///    installs. The break we send here is the cooperative path for any
///    backend that *does* install a Ctrl+Break handler. Failures and
///    non-Windows targets short-circuit silently and we proceed to the
///    hard kill.
/// 2. `kill_tree`: snapshot the PID *before* killing the direct child
///    so we can walk descendants. On Windows the direct child is
///    cmd.exe (BatBadBat wrapper, see `subprocess.rs`) and the real
///    agent (node.exe for codex / claude) is a grandchild — plain
///    `process.kill()` would only TerminateProcess the cmd.exe and the
///    orphan would keep writing to the console until our Job Object
///    closes, producing the multi-second hang users reported.
/// 3. `process.kill()` + `process.wait(2s)`: final TerminateProcess on
///    the direct handle and a bounded wait so we don't return while
///    the child is still draining.
fn teardown_interrupted_child(process: &running_process_core::NativeProcess) {
    if let Some(pid) = process.pid() {
        // Cooperative Ctrl+Break first. No-op on POSIX (returns false)
        // and any failure on Windows is non-fatal — the hard kill below
        // is always run regardless.
        let _ = process_tree::try_break_group(pid);
        process_tree::kill_tree(pid);
    }
    let _ = process.kill();
    let _ = process.wait(Some(std::time::Duration::from_secs(2)));
}

/// Inherited-stdio path: poll the child until it exits, kill on Ctrl+C.
/// This is the original `run_plan_subprocess` body, extracted unchanged so
/// the stream-json path can sit alongside it without duplicating the
/// non-streaming control flow.
fn run_with_inherited_stdio(
    process: &running_process_core::NativeProcess,
    interrupted: &AtomicBool,
) -> ProcessOutcome {
    loop {
        match process.poll() {
            Ok(Some(code)) => {
                if interrupted.load(Ordering::SeqCst) {
                    return ProcessOutcome::Interrupted;
                }
                return ProcessOutcome::Exited(code);
            }
            Ok(None) => {
                if interrupted.load(Ordering::SeqCst) {
                    teardown_interrupted_child(process);
                    return ProcessOutcome::Interrupted;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                eprintln!("[clud] error waiting for process: {}", e);
                return ProcessOutcome::Error;
            }
        }
    }
}

/// Stream-JSON path: drain captured stdout line-by-line, pipe each line
/// through `stream_json::render_line`, and print the rendered progress
/// line to our own stderr (so it shows alongside the existing
/// `[clud] iteration X/Y` banner and is easily distinguished from any
/// real claude stdout payload).
///
/// `captured_output` accumulates the **raw** lines (before stream-json
/// rendering) so the loop runner can scan for the
/// `<<<CLUD_LOOP_DONE: ...>>>` token fallback (issue #95). The raw form
/// is what the agent emits in a non-JSON-wrapped chunk; we keep the
/// payload as-is so token recognition isn't confused by event framing.
fn run_with_stream_json_renderer(
    process: &running_process_core::NativeProcess,
    interrupted: &AtomicBool,
    captured_output: &mut String,
) -> ProcessOutcome {
    use running_process_core::{ReadStatus, StreamKind};
    use std::time::Duration;

    let timeout = Duration::from_millis(100);
    loop {
        if interrupted.load(Ordering::SeqCst) {
            teardown_interrupted_child(process);
            return ProcessOutcome::Interrupted;
        }
        match process.read_stream(StreamKind::Stdout, Some(timeout)) {
            ReadStatus::Line(bytes) => {
                emit_rendered_line(&bytes, captured_output);
            }
            ReadStatus::Timeout => {
                // No new data within the window; check if the child has
                // exited so we don't spin forever after a slow turn.
                if let Ok(Some(_)) = process.poll() {
                    // Drain anything still queued before declaring done.
                    drain_remaining_stdout(process, captured_output);
                    break;
                }
            }
            ReadStatus::Eof => {
                break;
            }
        }
    }

    match process.wait(Some(Duration::from_secs(2))) {
        Ok(code) => ProcessOutcome::Exited(code),
        Err(_) => match process.returncode() {
            // EOF on the pipe doesn't imply the OS-level wait succeeded;
            // fall back to whatever the shared returncode tracker has.
            Some(code) => ProcessOutcome::Exited(code),
            None => ProcessOutcome::Exited(0),
        },
    }
}

fn drain_remaining_stdout(
    process: &running_process_core::NativeProcess,
    captured_output: &mut String,
) {
    use running_process_core::StreamKind;
    for chunk in process.drain_stream(StreamKind::Stdout) {
        emit_rendered_line(&chunk, captured_output);
    }
}

fn emit_rendered_line(bytes: &[u8], captured_output: &mut String) {
    let line = String::from_utf8_lossy(bytes);
    let trimmed = line.trim_end_matches(['\r', '\n']);
    // Issue #95: keep the raw text around so we can scan for the
    // `<<<CLUD_LOOP_DONE: ...>>>` token fallback after the iteration ends.
    captured_output.push_str(trimmed);
    captured_output.push('\n');
    if let Some(rendered) = stream_json::render_line(trimmed) {
        eprintln!("{rendered}");
    }
}

pub fn run_plan_pty(
    plan: &command::LaunchPlan,
    verbose: bool,
    interrupted: &AtomicBool,
    dnd_enabled: bool,
    mut loop_session: Option<&mut loop_artifacts::LoopSession>,
) -> i32 {
    use running_process_core::pty::NativePtyProcess;

    // Enable VT input on the Windows console for the whole PTY session.
    // The raw byte pump reads from clud's stdin, so PTY mode needs the
    // console to emit terminal-style bytes. In subprocess mode the child
    // inherits the console directly and must be allowed to configure input
    // modes itself.
    let _console_guard = enable_console_vt_input();

    // Issue #79 / #65 / #66: register the console IDropTarget for PTY
    // launches. The injector writes into `dnd_rx` which the pump drains
    // and forwards to the PTY master. Held for the full launch — the
    // refresh worker thread needs to keep displacing Claude Code's
    // own IDropTarget across iterations.
    #[cfg(windows)]
    let (_dnd_pty_guard, mut dnd_rx) = if dnd_enabled {
        crate::startup::try_register_console_drop_target_pty()
    } else {
        (None, None)
    };
    #[cfg(not(windows))]
    let (_dnd_pty_guard, mut dnd_rx): (Option<()>, Option<std::sync::mpsc::Receiver<Vec<u8>>>) = {
        let _ = dnd_enabled;
        (None, None)
    };

    let env = child_env();
    let mut last_exit = 0i32;
    let (rows, cols) = get_terminal_size();

    for iteration in 0..plan.iterations {
        // Re-check the interrupted flag at the top of every iteration. See
        // the matching guard in `run_plan_subprocess` — same rationale.
        if interrupted.load(Ordering::SeqCst) {
            eprintln!("[clud] interrupted via Ctrl+C");
            return 130;
        }

        let iter_num = iteration + 1;
        if plan.iterations > 1 {
            eprintln!("[clud] iteration {}/{}", iter_num, plan.iterations);
        }
        if let Some(s) = loop_session.as_deref_mut() {
            s.on_iteration_start(iter_num);
        }

        if verbose {
            eprintln!("[clud] exec (pty): {}", plan.command.join(" "));
        }

        let process = match NativePtyProcess::new(
            plan.command.clone(),
            plan.cwd.clone(),
            Some(env.clone()),
            rows,
            cols,
            None,
        ) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[clud] failed to create pty: {}", e);
                if let Some(s) = loop_session.as_deref_mut() {
                    s.on_iteration_end(iter_num, 1, Some(format!("pty create failed: {e}")));
                }
                return 1;
            }
        };

        // Echo off: the running-process-core PTY reader thread would
        // otherwise auto-write child output to our stdout via
        // `std::io::stdout().write_all`, bypassing our OSC filter. We
        // take chunks from `read_chunk_impl` inside the pump and run
        // them through `OscTitleStripper` before writing to stdout
        // ourselves.
        process.set_echo(false);

        if let Err(e) = process.start_impl() {
            eprintln!("[clud] failed to execute {}: {}", plan.command[0], e);
            if let Some(s) = loop_session.as_deref_mut() {
                s.on_iteration_end(iter_num, 1, Some(format!("pty start failed: {e}")));
            }
            return 1;
        }

        let mut hooks = voice::VoiceMode::from_env();
        let _raw_guard = session::enter_raw_mode_if_tty();
        // First iteration takes ownership of the dnd_rx (if any);
        // subsequent iterations get None. We can't clone the receiver
        // and the OLE registration is a one-shot for the whole
        // process anyway (see RefreshConfig — the worker thread is
        // shared across iterations).
        let extra_rx = if iteration == 0 { dnd_rx.take() } else { None };
        let exit_code = session::run_raw_pty_pump_with_extra_rx(
            &process,
            interrupted,
            &mut hooks,
            io::stdin(),
            extra_rx,
        );
        drop(_raw_guard);
        last_exit = normalize_exit_code(exit_code);
        if let Some(s) = loop_session.as_deref_mut() {
            let err = if last_exit == 130 {
                Some("Interrupted by user".to_string())
            } else {
                None
            };
            s.on_iteration_end(iter_num, last_exit, err);
        }

        if last_exit != 0 && plan.iterations > 1 {
            eprintln!(
                "[clud] iteration {} failed with exit code {}",
                iter_num, last_exit
            );
            return last_exit;
        }

        if let Some(code) = check_loop_markers(plan, iter_num) {
            return code;
        }
    }

    if let Some(code) = loop_unconverged_exit(plan) {
        return code;
    }

    last_exit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_terminal_size_uses_probe_when_present() {
        // Input is (cols, rows) from the terminal_size crate. Output is the
        // (rows, cols) pair we pass to NativePtyProcess::new.
        assert_eq!(resolve_terminal_size(Some((120, 40))), (40, 120));
    }

    #[test]
    fn resolve_terminal_size_caps_fallback_at_200_cols() {
        // Issue #31 T3: the previous `(24, 32767)` fallback blew up ratatui
        // layout math inside the child. The cap keeps us in normal terminal
        // territory.
        let (rows, cols) = resolve_terminal_size(None);
        assert_eq!(rows, 24);
        assert_eq!(cols, 200);
        assert!(cols <= 1024, "fallback cols must stay sane: {}", cols);
    }

    #[test]
    fn launch_mode_defaults_to_subprocess() {
        let launch_mode = crate::backend::LaunchMode::Subprocess;
        assert_eq!(launch_mode.as_str(), "subprocess");
    }
}
