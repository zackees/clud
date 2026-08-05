//! Subprocess- and PTY-mode runners for a single [`LaunchPlan`].
//!
//! These were inlined in `main.rs` until the file crossed 1k LOC. They
//! contain the per-iteration loop, the stream-json fallback, the
//! Ctrl-C-aware child teardown, and the launch-mode-specific wiring for
//! the OLE drag-drop registration.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::backend::Backend;
use crate::clud_settings;
use crate::command;
use crate::console_setup::enable_console_vt_input;
use crate::cpu_banner;
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
use crate::wedge_watchdog;
use crate::win_creation_flags;

#[path = "runner_exit.rs"]
mod runner_exit;
#[path = "runner_terminal.rs"]
mod runner_terminal;
use runner_exit::normalize_exit_code;

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

    // Issue #509: point the backend agent's temp dir at ~/.clud/tmp so its
    // scatter of temp files lands where the daemon can reclaim them. Empty
    // when disabled (CLUD_SESSION_TMP=0) or the dir can't be created, in
    // which case the child keeps the OS temp dir.
    for (key, value) in crate::gc::session_tmp::env_overrides() {
        push_or_replace(&mut env, &key, &value);
    }

    // Issue #753: keep Git-Bash completion functions out of the backend's
    // shell snapshot. Without this, every Bash tool call re-sources ~85
    // base64-decoded function definitions (~170 process spawns) before it
    // runs anything. See shell::completion_guard.
    for (key, value) in crate::shell::completion_guard::env_overrides() {
        push_or_replace(&mut env, &key, &value);
    }

    env
}

/// Wrap [`child_env`] with the per-backend shell policy from
/// `~/.clud/settings.json`. When `shell.disable_powershell` resolves true for
/// `backend` (issue #447):
///
/// - Both backends get `CLUD_DISABLE_POWERSHELL=1` so skills / CLAUDE.md
///   content can branch on it.
/// - Claude additionally gets `CLAUDE_CODE_USE_POWERSHELL_TOOL=0` (the
///   undocumented env-var kill-switch extracted from the bundled binary's
///   error strings) plus `CLAUDE_CODE_GIT_BASH_PATH` pointing at the lazily
///   resolved vendored Git Bash (see [`crate::shell::git_bash_resolver`]).
///   The PowerShell-tool toggle is set even if the resolver fails so Claude
///   surfaces a hard error instead of silently falling back to PowerShell.
/// - Codex has no equivalent env-var override (openai/codex#16717 is
///   closed). The Codex side ships as a PreToolUse hook in a follow-up PR;
///   here we just hand it `CLUD_DISABLE_POWERSHELL=1` for advisory use.
///
/// `Backend::Claude` is the case that actually changes behavior today.
pub fn child_env_for_backend(backend: Backend) -> Vec<(String, String)> {
    let home = clud_home_dir();
    child_env_for_backend_at(backend, home.as_deref())
}

/// Test seam — accepts the home dir explicitly so the policy can be exercised
/// against a temp directory without mutating the real `~/.clud/settings.json`.
pub fn child_env_for_backend_at(backend: Backend, home: Option<&Path>) -> Vec<(String, String)> {
    let mut env = child_env();
    let Some(home) = home else {
        return env;
    };

    let disable = match clud_settings::load_shell_disable_powershell_for_backend_at(home, backend) {
        Ok(value) => value,
        Err(_) => return env,
    };
    if !disable {
        return env;
    }

    push_or_replace(&mut env, "CLUD_DISABLE_POWERSHELL", "1");

    if !matches!(backend, Backend::Claude) {
        return env;
    }

    // The PowerShell-tool toggle is set unconditionally — if the resolver
    // below fails, Claude will hard-fail visibly with "Git Bash was not
    // found and the PowerShell tool is disabled" rather than silently
    // resurrecting PowerShell.
    push_or_replace(&mut env, "CLAUDE_CODE_USE_POWERSHELL_TOOL", "0");

    match crate::shell::git_bash_resolver::resolve_or_fetch_git_bash(home) {
        Ok(path) => {
            push_or_replace(
                &mut env,
                "CLAUDE_CODE_GIT_BASH_PATH",
                &path.to_string_lossy(),
            );
        }
        Err(error) => {
            eprintln!(
                "[clud] shell.disable_powershell=true but vendored bash fetch failed: {error}. \
                 Set CLAUDE_CODE_GIT_BASH_PATH to a bash.exe already on disk to recover."
            );
        }
    }

    env
}

fn push_or_replace(env: &mut Vec<(String, String)>, key: &str, value: &str) {
    env.retain(|(k, _)| k != key);
    env.push((key.to_string(), value.to_string()));
}

fn clud_home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(path) = std::env::var_os("USERPROFILE") {
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }
    if let Some(path) = std::env::var_os("HOME") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    None
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
    job_tracker: Option<&crate::job_orphan_reaper::ForegroundJobTracker>,
    verbose: bool,
    interrupted: &AtomicBool,
    mut loop_session: Option<&mut loop_artifacts::LoopSession>,
    cpu_banner_cfg: cpu_banner::CpuBannerCfg,
) -> i32 {
    use std::path::PathBuf;

    // Issue #466: CPU-burn banner. Watcher thread joins on drop at function
    // exit. Inert when cfg.enabled = false (no thread spawned).
    let _cpu_banner = cpu_banner::BannerWatcher::spawn(cpu_banner_cfg);

    let runtime = match crate::foreground_runtime::ForegroundRuntime::start(
        plan,
        child_env_for_backend(plan.backend),
    ) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("[clud] failed to start provider bridge: {error}");
            if verbose {
                verbose_log::log("[clud] provider bridge startup failed");
            }
            return 1;
        }
    };
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
        // Windows pipe-owning launches use `ManagedSubprocess`'s suspended
        // spawn: the child is assigned to its Job Object before it can run.
        // Console-attached launches and every non-Windows launch retain the
        // existing NativeProcess path and its Ctrl-C process-group behavior.
        let process = match runtime.spawn_subprocess(
            plan.command.clone(),
            plan.cwd.as_ref().map(PathBuf::from),
            plan.stream_json_progress,
            win_creation_flags::user_facing_backend_creationflags(),
        ) {
            Ok(process) => process,
            Err(e) => {
                eprintln!("[clud] failed to execute {}: {}", plan.command[0], e);
                if verbose {
                    verbose_log::log(format_args!("[clud] subprocess: start failed: {e}"));
                }
                if let Some(s) = loop_session.as_deref_mut() {
                    s.on_iteration_end(iter_num, 1, Some(format!("failed to start: {e}")));
                }
                return 1;
            }
        };
        if verbose {
            verbose_log::log("[clud] subprocess: started");
        }
        if let (Some(pid), Some(tracker)) = (process.pid(), job_tracker) {
            tracker.register_backend(pid, plan.backend.executable_name());
        }
        // Issue #541: wedge watchdog. Fresh per iteration (new pid each
        // time); dropped at the end of this loop body, which joins its
        // background thread promptly (see `WedgeWatchdog::stop`).
        let _wedge_watchdog = wedge_watchdog::WedgeWatchdog::spawn_for_pid(
            process.pid(),
            plan.backend.executable_name(),
        );

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
/// **Goal: sub-100ms return to shell.** The legacy path did a synchronous
/// `kill_tree` + bounded `process.wait(2s)`, which produced the user-
/// reported up-to-4s Ctrl+C lag (kill_tree's sysinfo refresh plus the
/// blocking wait; in the stream-JSON path the post-loop `process.wait(2s)`
/// could add a second 2s window). We now hand the root PID to the always-
/// on daemon over a fire-and-forget IPC, then return immediately and let
/// the kill-on-close Job Object (running-process-core 3.4+) TerminateProcess
/// the direct child as our process exits. `TerminateProcess` is synchronous
/// and silent — no signal, so cmd.exe never gets a chance to print
/// `Terminate batch job (Y/N)?`. If the daemon isn't available we fall
/// back to the old synchronous path (with the cooperative Ctrl+Break +
/// `kill_tree` + bounded wait) so `--no-daemon` users still get cleanup.
fn teardown_interrupted_child(process: &subprocess::ManagedSubprocess, batch_wrapped: bool) {
    if let Some(pid) = process.pid() {
        crate::ctrl_c_track::record_forensics(Some(pid));
        match crate::daemon::default_state_dir() {
            Ok(state_dir) => {
                if crate::daemon::try_handoff_kill_to_daemon(
                    &state_dir,
                    &[pid],
                    Some("ctrl_c_subprocess"),
                ) {
                    // The daemon will kill_tree on a background thread; our
                    // job is just to get out of the way so the user gets
                    // their shell back. The Job Object reaps the direct
                    // child via TerminateProcess as we exit.
                    crate::ctrl_c_track::record_handoff(true, Some("ctrl_c_subprocess"));
                    return;
                }
                crate::ctrl_c_track::record_handoff(false, Some("daemon_unreachable"));
            }
            Err(_) => {
                crate::ctrl_c_track::record_handoff(false, Some("no_state_dir"));
            }
        }
        // Daemon-less fallback: keep the legacy synchronous behavior so
        // `--no-daemon` invocations still leave the process tree clean.
        if process_tree::should_cooperative_break(batch_wrapped) {
            let _ = process_tree::try_break_group(pid);
        }
        process_tree::kill_tree(pid);
    } else {
        crate::ctrl_c_track::record_handoff(false, Some("no_child_pid"));
        crate::ctrl_c_track::record_forensics(None);
    }
    let _ = process.kill();
    // Daemon-less fallback only: bounded wait so the legacy path doesn't
    // race itself. Skipped when we successfully handed off above — that's
    // the whole point of the fast return.
    let _ = process.wait(Some(std::time::Duration::from_secs(2)));
}

/// Inherited-stdio path: poll the child until it exits, kill on Ctrl+C.
/// This is the original `run_plan_subprocess` body, extracted unchanged so
/// the stream-json path can sit alongside it without duplicating the
/// non-streaming control flow.
#[path = "runner_execution.rs"]
mod runner_execution;
use runner_execution::{run_with_inherited_stdio, run_with_stream_json_renderer};
