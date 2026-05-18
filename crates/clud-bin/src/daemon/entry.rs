use std::io;
use std::sync::atomic::AtomicBool;

use crate::args::{Args, Command};
use crate::backend::LaunchMode;
use crate::command::LaunchPlan;

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

pub fn experimental_enabled(args: &Args) -> bool {
    let repeat_enabled = matches!(
        args.command,
        Some(Command::Loop {
            repeat: Some(_),
            ..
        })
    );
    args.detach
        || args.detachable
        || repeat_enabled
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
    if let Err(err) = ensure_daemon(&state_dir) {
        eprintln!("[clud] failed to start daemon: {}", err);
        return 1;
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
            return 1;
        }
    };

    match response {
        DaemonResponse::Created { session } => {
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
