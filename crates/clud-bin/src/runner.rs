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
use crate::verbose_log;
use crate::voice;
use crate::win_creation_flags;

/// Merge two optional byte channels into one. Used by `run_plan_pty`
/// to combine the drag-drop side channel with the Windows console-input
/// reader (issue #141 follow-up) before handing the result to the
/// pump's `extra_rx` slot.
///
/// Zero or one input returns the inputs themselves (no extra thread).
/// Two inputs spawn a small forwarder thread per channel that drains
/// each input and forwards bytes to a unified output channel. The
/// forwarders exit when their input closes or the output drops.
fn merge_extra_rx(
    a: Option<std::sync::mpsc::Receiver<Vec<u8>>>,
    b: Option<std::sync::mpsc::Receiver<Vec<u8>>>,
) -> Option<std::sync::mpsc::Receiver<Vec<u8>>> {
    match (a, b) {
        (None, None) => None,
        (Some(rx), None) | (None, Some(rx)) => Some(rx),
        (Some(a), Some(b)) => {
            let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
            for input in [a, b] {
                let tx = tx.clone();
                std::thread::Builder::new()
                    .name("clud-extra-rx-merge".into())
                    .spawn(move || {
                        while let Ok(chunk) = input.recv() {
                            if tx.send(chunk).is_err() {
                                break;
                            }
                        }
                    })
                    .ok();
            }
            Some(rx)
        }
    }
}

/// Build the child environment: inherit parent env + inject tracking vars.
/// Deduplicates keys so we never pass the same var twice.
///
/// On Windows, also forces UTF-8 for any Python helper the agent shells
/// out to (Codex / Claude tool scripts, MCP servers, install probes …)
/// so output doesn't mojibake against the user's OEM codepage. Paired
/// with the `chcp 65001` prefix in `subprocess::render_windows_batch_command`
/// (issue #168). Node itself respects the console codepage and needs no
/// dedicated env var.
pub fn child_env() -> Vec<(String, String)> {
    let originator_key = running_process::ORIGINATOR_ENV_VAR;

    let utf8_keys: &[&str] = if cfg!(windows) {
        &["IN_CLUD", originator_key, "PYTHONIOENCODING", "PYTHONUTF8"]
    } else {
        &["IN_CLUD", originator_key]
    };

    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| !utf8_keys.contains(&k.as_str()))
        .collect();

    env.push(("IN_CLUD".to_string(), "1".to_string()));

    let originator_value = format!("CLUD:{}", std::process::id());
    env.push((originator_key.to_string(), originator_value));

    if cfg!(windows) {
        env.push(("PYTHONIOENCODING".to_string(), "utf-8".to_string()));
        env.push(("PYTHONUTF8".to_string(), "1".to_string()));
    }

    env
}

pub fn get_terminal_size() -> (u16, u16) {
    let probe = terminal_size::terminal_size().map(|(w, h)| (w.0, h.0));
    resolve_terminal_size(probe)
}

fn display_verbose_command(command: &[String]) -> String {
    let Some((program, args)) = command.split_first() else {
        return String::new();
    };
    let mut rendered = Vec::with_capacity(command.len());
    rendered.push(display_program_name(program));
    rendered.extend(args.iter().cloned());
    rendered.join(" ")
}

fn display_program_name(program: &str) -> String {
    let tail = program.rsplit(['\\', '/']).next().unwrap_or(program);
    if tail.is_empty() {
        program.to_string()
    } else {
        tail.to_string()
    }
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

    use running_process::{NativeProcess, ProcessConfig, StderrMode, StdinMode};

    let env = child_env();
    let mut last_exit = 0i32;

    for iteration in 0..plan.iterations {
        // Re-check the interrupted flag at the top of every iteration. A
        // Ctrl+C that fires between the previous child's reap and our next
        // spawn would otherwise be silently swallowed and we'd cheerfully
        // launch another codex run. 130 is the conventional SIGINT exit
        // code and mirrors what `ProcessOutcome::Interrupted` produces.
        if interrupted.load(Ordering::SeqCst) {
            if verbose {
                verbose_log::log("[clud] interrupted via Ctrl+C");
            }
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
            verbose_log::log(format_args!(
                "[clud] exec (subprocess): {}",
                display_verbose_command(&plan.command)
            ));
        }

        let batch_wrapped = subprocess::argv_is_batch_wrapped(&plan.command);
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
            // npm test, long builds) that leak as zombies when clud dies
            // abnormally. Since `running-process-core` 3.4, every
            // `NativeProcess` is automatically placed in a kill-on-close
            // Job Object on Windows, which gives us the same blast-radius
            // bound as the old `Containment::Contained` opt-in. On Linux
            // the wrapper no longer sets `PR_SET_PDEATHSIG(SIGKILL)`
            // implicitly; orphan reaping there falls back to the daemon
            // worker's `pid_is_alive` watchdog (foreground sessions still
            // rely on the OS killing the process tree at terminal close).
        };

        let process = NativeProcess::new(config);
        if let Err(e) = process.start() {
            eprintln!("[clud] failed to execute {}: {}", plan.command[0], e);
            if verbose {
                verbose_log::log(format_args!("[clud] subprocess: start failed: {e}"));
            }
            if let Some(s) = loop_session.as_deref_mut() {
                s.on_iteration_end(iter_num, 1, Some(format!("failed to start: {e}")));
            }
            return 1;
        }
        if verbose {
            verbose_log::log("[clud] subprocess: started");
        }

        // Issue #95: in stream-json mode we also accumulate the rendered
        // output so we can fall back to scanning for the
        // `<<<CLUD_LOOP_DONE: ...>>>` token if the agent skipped the
        // marker file. In inherited-stdio mode the child writes directly
        // to the user's terminal and we never see the bytes — the token
        // fallback is unavailable there.
        let mut captured_output = String::new();
        let exit_code = if plan.stream_json_progress {
            run_with_stream_json_renderer(
                &process,
                interrupted,
                &mut captured_output,
                batch_wrapped,
            )
        } else {
            run_with_inherited_stdio(&process, interrupted, batch_wrapped)
        };
        match exit_code {
            ProcessOutcome::Exited(code) => {
                last_exit = code;
                if verbose {
                    verbose_log::log(format_args!("[clud] subprocess: exited code {code}"));
                }
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
                if verbose {
                    verbose_log::log("[clud] interrupted via Ctrl+C");
                }
                if let Some(s) = loop_session.as_deref_mut() {
                    s.on_iteration_end(iter_num, 130, Some("Interrupted by user".to_string()));
                }
                return 130;
            }
            ProcessOutcome::Error => {
                if verbose {
                    verbose_log::log("[clud] subprocess: runner error");
                }
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
/// 1. Windows-only, when the direct child is a native executable: send
///    `CTRL_BREAK_EVENT` to the child's console process group. The
///    backend is spawned with
///    `CREATE_NEW_PROCESS_GROUP` so the OS does *not* deliver the
///    user's `CTRL_C_EVENT` to it — that prevents stray
///    `KeyboardInterrupt` tracebacks from blocking `subprocess` waits
///    in the `nodejs-wheel` Python launcher used by some Claude Code
///    installs. The break we send here is the cooperative path for any
///    backend that *does* install a Ctrl+Break handler. Failures and
///    non-Windows targets short-circuit silently and we proceed to the
///    hard kill.
///
///    When the direct child is `cmd.exe` running a `.cmd` / `.bat`
///    backend wrapper, skip this step. Ctrl+Break makes cmd's batch
///    interpreter display `Terminate batch job (Y/N)?` and wait on
///    stdin, so the clean interrupt path must go straight to `kill_tree`.
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
fn teardown_interrupted_child(process: &running_process::NativeProcess, batch_wrapped: bool) {
    if let Some(pid) = process.pid() {
        // Cooperative Ctrl+Break first when it is safe. No-op on POSIX
        // (returns false), and any Windows failure is non-fatal because
        // the hard kill below always runs.
        if process_tree::should_cooperative_break(batch_wrapped) {
            let _ = process_tree::try_break_group(pid);
        }
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
    process: &running_process::NativeProcess,
    interrupted: &AtomicBool,
    batch_wrapped: bool,
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
                    teardown_interrupted_child(process, batch_wrapped);
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
    process: &running_process::NativeProcess,
    interrupted: &AtomicBool,
    captured_output: &mut String,
    batch_wrapped: bool,
) -> ProcessOutcome {
    use running_process::{ReadStatus, StreamKind};
    use std::time::Duration;

    let timeout = Duration::from_millis(100);
    loop {
        if interrupted.load(Ordering::SeqCst) {
            teardown_interrupted_child(process, batch_wrapped);
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

fn drain_remaining_stdout(process: &running_process::NativeProcess, captured_output: &mut String) {
    use running_process::StreamKind;
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
    use running_process::pty::NativePtyProcess;

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

    // Issue #141 follow-up: spawn the Windows console-input reader so
    // Shift+Enter inserts `\n` even under bare cmd.exe in conhost. The
    // reader produces a `Receiver<Vec<u8>>` we merge with `dnd_rx` for
    // the pump's first iteration. Held for the whole function so the
    // console mode is restored on Drop. Skipped if stdin isn't a real
    // console (the reader can't function on a piped stdin).
    #[cfg(windows)]
    let (mut console_input_rx, _console_input_guards) = if session::terminals_are_interactive() {
        match crate::console_input::spawn_console_input_reader() {
            Ok((mut handle, mode_guard)) => {
                let rx = handle.take_receiver();
                (rx, Some((handle, mode_guard)))
            }
            Err(e) => {
                eprintln!("[clud] note: console-input reader unavailable: {e}");
                (None, None)
            }
        }
    } else {
        (None, None)
    };
    #[cfg(not(windows))]
    let (mut console_input_rx, _console_input_guards): (
        Option<std::sync::mpsc::Receiver<Vec<u8>>>,
        Option<()>,
    ) = (None, None);

    let env = child_env();
    let mut last_exit = 0i32;
    let terminal_capabilities = (plan.graphics.mode != crate::graphics::GraphicsMode::Off)
        .then(crate::graphics::detect_current_terminal);
    let graphics_decision =
        crate::graphics::decide_sixel(&plan.graphics, terminal_capabilities.as_ref());
    if verbose {
        verbose_log::log(format_args!(
            "[clud] graphics: {} ({})",
            graphics_decision.reason,
            crate::graphics::capability_summary(terminal_capabilities.as_ref())
        ));
    }

    for iteration in 0..plan.iterations {
        let (terminal_rows, cols) = get_terminal_size();
        let mut rows = terminal_rows;
        let header = if graphics_decision.enabled {
            match crate::graphics::render_header(&plan.graphics, terminal_rows, cols) {
                Ok(Some(header)) => {
                    rows = header.text_rows;
                    Some(header)
                }
                Ok(None) => {
                    if verbose {
                        verbose_log::log(format_args!(
                            "[clud] graphics: skipped header for terminal rows={terminal_rows} cols={cols}"
                        ));
                    }
                    None
                }
                Err(err) => {
                    eprintln!("[clud] warning: failed to render graphics header: {err}");
                    if verbose {
                        verbose_log::log(format_args!("[clud] graphics: render failed: {err}"));
                    }
                    None
                }
            }
        } else {
            None
        };
        if verbose {
            verbose_log::log(format_args!(
                "[clud] pty: terminal size rows={terminal_rows} cols={cols} pty_rows={rows}"
            ));
        }

        // Re-check the interrupted flag at the top of every iteration. See
        // the matching guard in `run_plan_subprocess` — same rationale.
        if interrupted.load(Ordering::SeqCst) {
            if verbose {
                verbose_log::log("[clud] interrupted via Ctrl+C (pty)");
            }
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
            verbose_log::log(format_args!(
                "[clud] exec (pty): {}",
                display_verbose_command(&plan.command)
            ));
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
                if verbose {
                    verbose_log::log(format_args!("[clud] pty: create failed: {e}"));
                }
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
            if verbose {
                verbose_log::log(format_args!("[clud] pty: start failed: {e}"));
            }
            if let Some(s) = loop_session.as_deref_mut() {
                s.on_iteration_end(iter_num, 1, Some(format!("pty start failed: {e}")));
            }
            return 1;
        }
        if verbose {
            verbose_log::log("[clud] pty: started");
        }

        if let Some(header) = &header {
            write_terminal_bytes(&header.bytes);
        }
        let header_restore = header.as_ref().map(|header| header.restore_bytes.clone());
        let graphics_resize = header.as_ref().map(|_| plan.graphics.clone());
        let mut hooks = voice::VoiceMode::from_env();
        let _raw_guard = session::enter_raw_mode_if_tty();
        // First iteration takes ownership of the side-channel receivers
        // (drag-drop bytes and, on Windows, console-input bytes from
        // the Shift+Enter reader); subsequent iterations get None. We
        // can't clone an `mpsc::Receiver`, and the OLE registration is
        // a one-shot for the whole process anyway (see `RefreshConfig`
        // — the worker thread is shared across iterations). The console
        // reader behaves similarly: its worker keeps running for the
        // life of `run_plan_pty`, but only iteration 0 sees its bytes.
        // For typical single-iteration invocations this is irrelevant;
        // for `clud loop` it's an accepted limitation tracked in the
        // PR body.
        let extra_rx = if iteration == 0 {
            merge_extra_rx(dnd_rx.take(), console_input_rx.take())
        } else {
            None
        };
        let exit_code = session::run_raw_pty_pump_with_extra_rx_verbose_and_graphics(
            &process,
            interrupted,
            &mut hooks,
            io::stdin(),
            extra_rx,
            verbose,
            graphics_resize,
        );
        drop(_raw_guard);
        if let Some(bytes) = header_restore.as_deref() {
            write_terminal_bytes(bytes);
        }
        last_exit = normalize_exit_code(exit_code);
        if verbose {
            verbose_log::log(format_args!("[clud] pty: exited code {last_exit}"));
        }
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

fn write_terminal_bytes(bytes: &[u8]) {
    use std::io::Write;
    let mut out = io::stdout().lock();
    let _ = out.write_all(bytes);
    let _ = out.flush();
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

    #[test]
    fn display_verbose_command_strips_program_paths() {
        let command = vec![
            r"C:\tools\node\claude.exe".to_string(),
            "--verbose".to_string(),
            "-p".to_string(),
            "hello".to_string(),
        ];
        assert_eq!(
            display_verbose_command(&command),
            "claude.exe --verbose -p hello"
        );

        let command = vec![
            "/usr/local/bin/codex".to_string(),
            "--dangerously-bypass-approvals-and-sandbox".to_string(),
        ];
        assert_eq!(
            display_verbose_command(&command),
            "codex --dangerously-bypass-approvals-and-sandbox"
        );
    }

    #[test]
    fn display_verbose_command_keeps_plain_program_names() {
        let command = vec![
            "claude".to_string(),
            "--model".to_string(),
            "opus".to_string(),
        ];
        assert_eq!(display_verbose_command(&command), "claude --model opus");
    }

    /// Issue #168: Windows children get UTF-8 forced via Python env vars
    /// so any Python helper the agent spawns emits and reads UTF-8.
    /// IN_CLUD and ORIGINATOR vars must still be present, and PYTHONUTF8
    /// must be exactly "1" (Python accepts 0/1 only).
    #[cfg(windows)]
    #[test]
    fn child_env_sets_python_utf8_vars_on_windows() {
        let env = child_env();
        let lookup = |key: &str| -> Option<String> {
            env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
        };
        assert_eq!(lookup("PYTHONIOENCODING").as_deref(), Some("utf-8"));
        assert_eq!(lookup("PYTHONUTF8").as_deref(), Some("1"));
        assert_eq!(lookup("IN_CLUD").as_deref(), Some("1"));
        assert!(
            lookup(running_process::ORIGINATOR_ENV_VAR).is_some(),
            "ORIGINATOR var must still be set"
        );
        let pyio_count = env.iter().filter(|(k, _)| k == "PYTHONIOENCODING").count();
        assert_eq!(pyio_count, 1, "PYTHONIOENCODING must appear exactly once");
    }
}
