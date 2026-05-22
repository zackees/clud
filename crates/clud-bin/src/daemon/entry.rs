use std::io::{self, IsTerminal};
use std::sync::atomic::AtomicBool;

use crate::args::{Args, Command};
use crate::backend::LaunchMode;
use crate::command::LaunchPlan;
use crate::verbose_log;

use super::attach::{attach_to_session, run_attach};
use super::client::{ensure_daemon, send_daemon_request};
use super::commands::{run_kill, run_list, run_logs};
use super::io_helpers::{resolve_backlog_bytes, terminal_dimensions};
use super::paths::state_dir;
use super::server::run_daemon;
use super::sessions::{most_recent_session, most_recent_session_any};
use super::types::{
    DaemonRequest, DaemonResponse, SessionKind, WorkerLaunchSpec, ENV_FEATURE_FLAG,
};
use super::worker::run_worker;
use crate::gc_daemon::ENV_NO_DAEMON;

/// True when the launch should be routed through the centralized session
/// daemon (`daemon::run_centralized_session`) instead of the direct
/// runner in `runner::run_plan_{subprocess,pty}`.
///
/// **As of PR3 the centralized path is now the default for *interactive*
/// launches** — when both stdin and stdout are TTYs. Piped invocations
/// (`clud -p "x" | jq`, `echo foo | clud`, CI test harnesses) keep using
/// the direct runner so they don't pay the daemon round-trip and so
/// stdio framing stays byte-identical with what scripts expect.
///
/// Override matrix:
///
/// | Trigger                                  | Centralized? |
/// |------------------------------------------|--------------|
/// | `--detach` / `--detachable` / repeat job | **forced on** |
/// | `--experimental-daemon-centralized`      | **forced on** (legacy alias) |
/// | `CLUD_EXPERIMENTAL_DAEMON=1`             | **forced on** (legacy alias) |
/// | `--no-daemon`                            | off |
/// | `CLUD_NO_DAEMON=1`                       | off |
/// | Interactive (stdin & stdout both TTYs)   | **on** ← new default |
/// | Piped (anything else)                    | off (direct runner) |
///
/// The legacy "experimental" opt-in still forces centralized so any
/// script that was setting `CLUD_EXPERIMENTAL_DAEMON=1` keeps working
/// unchanged. The function name `experimental_enabled` is preserved for
/// the same reason (one external call site in `main.rs`); a rename can
/// land as a follow-up cleanup.
pub fn experimental_enabled(args: &Args) -> bool {
    let repeat_enabled = matches!(
        args.command,
        Some(Command::Loop {
            repeat: Some(_),
            ..
        })
    );

    let force_on = args.detach
        || args.detachable
        || repeat_enabled
        || args.experimental_daemon_centralized
        || env_truthy(ENV_FEATURE_FLAG);
    if force_on {
        return true;
    }

    // PR3: centralized daemon is now the default for interactive launches.
    // Honor the user's explicit opt-out flags first.
    if args.no_daemon || env_truthy(ENV_NO_DAEMON) {
        return false;
    }

    // Interactive (both TTYs) → centralized. Piped → direct runner.
    //
    // Rationale: every meaningful win of the centralized path (kill-on-
    // close Job Object lifetime, attach/detach, replay, session listing,
    // voice + DnD) only matters when there's a human at the keyboard.
    // For piped one-shots the direct runner is simpler, faster, and
    // produces byte-identical stdio framing that script automation
    // relies on.
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
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
            let sessions = super::sessions::list_attachable_sessions(&state_dir);
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
        Some(Command::Logs {
            session_id,
            follow,
            lines,
            last,
        }) => {
            let state_dir = state_dir(args);
            // `--last` resolves to the most recently created session,
            // including exited ones — logs are valuable post-mortem.
            let resolved_id: Option<String> = if *last {
                match most_recent_session_any(&state_dir) {
                    Some(session) => {
                        eprintln!(
                            "[clud] showing logs for most recent session: {}",
                            session.id
                        );
                        Some(session.id)
                    }
                    None => {
                        eprintln!("[clud] no sessions found");
                        return Some(1);
                    }
                }
            } else {
                session_id.clone()
            };
            Some(run_logs(
                &state_dir,
                resolved_id.as_deref(),
                *follow,
                *lines,
                interrupted,
            ))
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
    if args.verbose {
        verbose_log::log(format_args!(
            "[clud] daemon: ensure state_dir={}",
            verbose_log::display_path(&state_dir)
        ));
    }
    if let Err(err) = ensure_daemon(&state_dir) {
        eprintln!("[clud] failed to start daemon: {}", err);
        if args.verbose {
            verbose_log::log(format_args!("[clud] daemon: ensure failed: {err}"));
        }
        return 1;
    }
    if args.verbose {
        verbose_log::log("[clud] daemon: ready");
    }

    let repeat_enabled = plan.repeat_schedule.is_some();
    let kind = if repeat_enabled {
        SessionKind::Subprocess
    } else {
        match plan.launch_mode {
            LaunchMode::Subprocess => SessionKind::Subprocess,
            LaunchMode::Pty => SessionKind::Pty,
        }
    };
    let (rows, cols) = terminal_dimensions();
    let backlog_bytes = resolve_backlog_bytes(args.backlog_size.as_deref());
    let name = args
        .session_name
        .clone()
        .or_else(|| repeat_enabled.then(|| plan.task_summary.clone()).flatten());
    let repeat_run_command = if repeat_enabled {
        match build_repeat_once_command(args) {
            Ok(command) => Some(command),
            Err(err) => {
                eprintln!("[clud] failed to build repeat command: {}", err);
                return 1;
            }
        }
    } else {
        None
    };
    if args.verbose {
        verbose_log::log(format_args!(
            "[clud] daemon: create session kind={:?} detach={} repeat={}",
            kind,
            args.detach || args.detachable,
            repeat_enabled
        ));
    }
    let request = DaemonRequest::Create {
        spec: Box::new(WorkerLaunchSpec {
            plan: plan.clone(),
            kind,
            name,
            detachable: args.detach || args.detachable,
            background_on_launch: args.detach || repeat_enabled,
            attachable: !repeat_enabled,
            rows,
            cols,
            repeat_interval_secs: plan.repeat_schedule.as_ref().map(|s| s.interval_secs),
            repeat_run_command,
            backlog_bytes,
        }),
    };
    let response = match send_daemon_request(&state_dir, &request) {
        Ok(response) => response,
        Err(err) => {
            eprintln!("[clud] daemon request failed: {}", err);
            if args.verbose {
                verbose_log::log(format_args!("[clud] daemon: request failed: {err}"));
            }
            return 1;
        }
    };

    match response {
        DaemonResponse::Created { session } => {
            if args.verbose {
                verbose_log::log(format_args!("[clud] daemon: session {}", session.id));
            }
            if repeat_enabled {
                eprintln!("[clud] repeat job {} running in background", session.id);
                eprintln!("[clud] list jobs with: clud list");
                return 0;
            }
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
            if args.verbose {
                verbose_log::log(format_args!("[clud] daemon: error: {message}"));
            }
            1
        }
        DaemonResponse::Session { .. } | DaemonResponse::Terminated { .. } => 1,
    }
}

fn build_repeat_once_command(args: &Args) -> io::Result<Vec<String>> {
    let exe = std::env::current_exe()?;
    let mut command = vec![exe.to_string_lossy().to_string()];
    if args.codex {
        command.push("--codex".to_string());
    } else if args.claude {
        command.push("--claude".to_string());
    }
    if args.safe {
        command.push("--safe".to_string());
    }
    if args.subprocess {
        command.push("--subprocess".to_string());
    }
    if args.pty {
        command.push("--pty".to_string());
    }
    if args.verbose {
        command.push("--verbose".to_string());
    }
    if let Some(model) = args.model.as_deref() {
        command.push("--model".to_string());
        command.push(model.to_string());
    }
    command.push("loop".to_string());
    if let Some(Command::Loop {
        task,
        loop_count,
        refresh,
        no_done,
        done,
        ..
    }) = &args.command
    {
        command.push("--loop-count".to_string());
        command.push(loop_count.to_string());
        if *refresh {
            command.push("--refresh".to_string());
        }
        if *no_done || done.is_none() {
            command.push("--no-done".to_string());
        }
        if let Some(path) = done.as_deref() {
            command.push("--done".to_string());
            command.push(path.to_string());
        }
        if let Some(task) = task.as_deref() {
            command.push(task.to_string());
        }
    }
    if !args.passthrough.is_empty() {
        command.push("--".to_string());
        command.extend(args.passthrough.iter().cloned());
    }
    Ok(command)
}
