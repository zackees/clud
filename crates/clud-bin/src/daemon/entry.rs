use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::args::{Args, Command, DaemonSubcommand};
use crate::backend::LaunchMode;
use crate::command::{has_noninteractive_prompt, LaunchPlan};
use crate::verbose_log;

use super::attach::{attach_to_session, run_attach};
use super::client::{ensure_daemon, probe_existing, request_daemon_shutdown, send_daemon_request};
use super::commands::{run_kill, run_list, run_logs};
use super::io_helpers::{read_json_file, resolve_backlog_bytes, terminal_dimensions};
use super::paths::{daemon_info_path, state_dir};
use super::server::run_daemon;
use super::sessions::{most_recent_session, most_recent_session_any};
use super::types::{
    DaemonInfo, DaemonRequest, DaemonResponse, SessionKind, WorkerLaunchSpec, ENV_FEATURE_FLAG,
};
use super::worker::run_worker;

const RUNNING_PROCESS_SERVICE_NAME: &str = "clud";
const RUNNING_PROCESS_SERVICE_DEF_DIR_ENV: &str = "RUNNING_PROCESS_SERVICE_DEF_DIR";
const RUNNING_PROCESS_DISABLE_ENV: &str = "RUNNING_PROCESS_DISABLE";
const RUNNING_PROCESS_BROKER_ENV: &str = "CLUD_RUNNING_PROCESS_BROKER";

/// True when the launch should be routed through the centralized session
/// daemon (`daemon::run_centralized_session`) instead of the direct
/// runner in `runner::run_plan_{subprocess,pty}`.
///
/// The centralized path is **opt-in**. Defaulting it on for interactive
/// launches (the PR #151 experiment) exposed a latent bug: the attach
/// pump (`run_remote_interactive`) reads stdin through `crossterm::event`,
/// which drops DSR / DA / OSC replies the child TUI writes on startup
/// (same lossy-demultiplexer issue #46 already fixed for the local-PTY
/// runner). With nothing answering claude's `\x1b[6n` query, the TUI
/// hangs and the user sees a blank screen. Until the attach pump is
/// rewritten to forward raw stdin bytes (like `run_raw_pty_pump` does),
/// the safe default is to leave plain `clud` on the direct runner.
///
/// Override matrix:
///
/// | Trigger                                  | Centralized? |
/// |------------------------------------------|--------------|
/// | `--detach` / `--detachable` / repeat job | **forced on** |
/// | `--transcript <path>`                    | **forced on** |
/// | `--experimental-daemon-centralized`      | **forced on** (legacy alias) |
/// | `CLUD_EXPERIMENTAL_DAEMON=1`             | **forced on** (legacy alias) |
/// | `--no-daemon` / `CLUD_NO_DAEMON=1`       | off (no-ops here, kept for explicitness) |
/// | Everything else                          | off (direct runner) |
///
/// The function name `experimental_enabled` is preserved for back-compat
/// (one external call site in `main.rs`); a rename can land in a follow-up.
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
        || args.transcript.is_some()
        || repeat_enabled
        || args.experimental_daemon_centralized
        || env_truthy(ENV_FEATURE_FLAG)
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn run_daemon_subcommand(state_dir: &Path, subcommand: &DaemonSubcommand) -> i32 {
    match subcommand {
        DaemonSubcommand::Restart => match request_daemon_shutdown(state_dir) {
            Ok(pid) => {
                eprintln!("[clud] daemon pid {pid} stopped");
                if let Err(err) = ensure_daemon(state_dir) {
                    eprintln!("[clud] failed to start replacement daemon: {err}");
                    return 1;
                }
                eprintln!("[clud] new daemon started");
                0
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                eprintln!("[clud] no running daemon; starting one");
                if let Err(err) = ensure_daemon(state_dir) {
                    eprintln!("[clud] failed to start daemon: {err}");
                    return 1;
                }
                0
            }
            Err(err) => {
                eprintln!("[clud] daemon restart failed: {err}");
                1
            }
        },
        DaemonSubcommand::RunningProcess { json } => {
            run_running_process_diagnostics(state_dir, *json)
        }
    }
}

fn run_running_process_diagnostics(state_dir: &Path, json: bool) -> i32 {
    let service_def_dir = running_process_service_def_dir();
    let service_def_path =
        service_def_dir.join(format!("{RUNNING_PROCESS_SERVICE_NAME}.servicedef"));
    let daemon_info_path = daemon_info_path(state_dir);
    let recorded_daemon = read_json_file::<DaemonInfo>(&daemon_info_path).ok();
    let live_daemon = probe_existing(state_dir);
    let current_exe = std::env::current_exe().ok();
    let broker_requested = env_flag_eq_one(RUNNING_PROCESS_BROKER_ENV);
    let broker_disabled = env_flag_eq_one(RUNNING_PROCESS_DISABLE_ENV);
    let servicedef_installed = service_def_path.exists();
    let wire_mode = super::rp_broker::WireMode::select();
    let mode = if broker_disabled {
        "disabled-direct-daemon"
    } else {
        "frame-lane-with-tcp-fallback"
    };
    let summary = if broker_disabled {
        "RUNNING_PROCESS_DISABLE=1 selects json-legacy + the direct TCP daemon endpoint; the broker frame lane is bypassed."
    } else {
        "Clud serves a running-process broker v1 frame lane (payload protocol 0x7C4C) next to its TCP wire; the client adopts the broker session (BrokerSession::adopt) and falls back to legacy JSON over TCP on any miss."
    };
    let deferred = [
        "broker-spawned backend adoption (the clud daemon remains self-managed)",
        "Phase 8 escape-hatch removal",
    ];

    if json {
        let payload = serde_json::json!({
            "service_name": RUNNING_PROCESS_SERVICE_NAME,
            "service_definition": {
                "file_name": format!("{RUNNING_PROCESS_SERVICE_NAME}.servicedef"),
                "directory": path_string(&service_def_dir),
                "path": path_string(&service_def_path),
                "directory_env_override": RUNNING_PROCESS_SERVICE_DEF_DIR_ENV,
                "isolation": "SHARED_BROKER",
                "min_version": super::rp_broker::RUNNING_PROCESS_MIN_VERSION,
                "installed_by_clud": servicedef_installed,
                "status": if servicedef_installed { "installed" } else { "pending_first_daemon_bringup" },
            },
            "daemon": {
                "state_dir": path_string(state_dir),
                "info_path": path_string(&daemon_info_path),
                "recorded": daemon_info_json(recorded_daemon.as_ref()),
                "live_reachable": live_daemon.is_some(),
                "recorded_version_matches_current": recorded_daemon
                    .as_ref()
                    .map(|info| info.version.as_deref() == Some(env!("CARGO_PKG_VERSION"))),
                "current_binary": current_exe.as_ref().map(|path| path_string(path)),
            },
            "mode": {
                "current": mode,
                "wire_mode": wire_mode.as_str(),
                "summary": summary,
                "uses_direct_daemon_fallback": broker_disabled,
                "broker_client_wired": !broker_disabled,
                "adopts_broker_session": !broker_disabled,
            },
            "environment": {
                "RUNNING_PROCESS_DISABLE": broker_disabled,
                "CLUD_RUNNING_PROCESS_BROKER": broker_requested,
            },
            "deferred": deferred,
        });
        match serde_json::to_string_pretty(&payload) {
            Ok(text) => println!("{text}"),
            Err(err) => {
                eprintln!("[clud] failed to render running-process diagnostics: {err}");
                return 1;
            }
        }
    } else {
        println!("running-process adoption status for clud");
        println!("service: {RUNNING_PROCESS_SERVICE_NAME}");
        println!("isolation: SHARED_BROKER");
        println!(
            "min_version: {}",
            super::rp_broker::RUNNING_PROCESS_MIN_VERSION
        );
        println!("servicedef: {}", service_def_path.display());
        println!("servicedef installed: {servicedef_installed}");
        println!("daemon state: {}", state_dir.display());
        println!("daemon info: {}", daemon_info_path.display());
        println!("live daemon reachable: {}", live_daemon.is_some());
        println!("mode: {mode}");
        println!("wire_mode: {}", wire_mode.as_str());
        println!("{summary}");
        println!("deferred:");
        for item in deferred {
            println!("- {item}");
        }
    }

    0
}

fn daemon_info_json(info: Option<&DaemonInfo>) -> serde_json::Value {
    match info {
        Some(info) => serde_json::json!({
            "pid": info.pid,
            "port": info.port,
            "dashboard_port": info.dashboard_port,
            "version": info.version.as_deref(),
        }),
        None => serde_json::Value::Null,
    }
}

fn env_flag_eq_one(name: &str) -> bool {
    std::env::var(name)
        .map(|value| value == "1")
        .unwrap_or(false)
}

/// Single source of truth: running-process's own resolver (honors the
/// `RUNNING_PROCESS_SERVICE_DEF_DIR` override, then platform defaults).
/// The daemon writes `clud.servicedef` into the same directory at
/// bringup (`rp_broker::install_service_definition`).
fn running_process_service_def_dir() -> PathBuf {
    running_process::broker::server::service_definition_dir()
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
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
        Some(Command::Daemon { subcommand }) => {
            let state_dir = state_dir(args);
            Some(run_daemon_subcommand(&state_dir, subcommand))
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

/// Pick the worker's `SessionKind` for a centralized-daemon launch.
///
/// The daemon worker's subprocess path wires the backend's stdin to a
/// NULL handle (see `worker::start_subprocess_session`). For interactive
/// claude that's fatal: claude detects no TTY and drops into its built-in
/// `--print` mode, which requires a prompt and errors otherwise
/// ("Input must be provided either through stdin or as a prompt
/// argument when using --print"). The direct runner avoided this by
/// inheriting clud's TTY; the daemon worker can't because the user's
/// terminal belongs to the foreground attach client, not the long-lived
/// worker. Force PTY for interactive launches so the backend gets a
/// pseudo-terminal it can drive.
///
/// `repeat_enabled` keeps overriding to subprocess — repeat jobs run in
/// the background, have their own prompt embedded, and never need a TTY.
fn select_session_kind(
    plan_mode: LaunchMode,
    repeat_enabled: bool,
    noninteractive_prompt: bool,
) -> SessionKind {
    if repeat_enabled {
        return SessionKind::Subprocess;
    }
    if !noninteractive_prompt {
        return SessionKind::Pty;
    }
    match plan_mode {
        LaunchMode::Subprocess => SessionKind::Subprocess,
        LaunchMode::Pty => SessionKind::Pty,
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
    let transcript_path = match args.transcript.as_deref() {
        Some(path) => match prepare_transcript_path(path) {
            Ok(path) => Some(path),
            Err(err) => {
                eprintln!(
                    "[clud] failed to prepare transcript {}: {}",
                    path.display(),
                    err
                );
                return 1;
            }
        },
        None => None,
    };
    let kind = select_session_kind(
        plan.launch_mode,
        repeat_enabled,
        has_noninteractive_prompt(args),
    );
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
            transcript_path,
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
        DaemonResponse::Session { .. }
        | DaemonResponse::Terminated { .. }
        | DaemonResponse::Interrupted { .. }
        | DaemonResponse::AdoptKillAck { .. }
        | DaemonResponse::Gc { .. }
        | DaemonResponse::LiveCwds { .. }
        | DaemonResponse::ShutdownAck { .. } => 1,
    }
}

fn prepare_transcript_path(path: &Path) -> io::Result<PathBuf> {
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if let Some(parent) = resolved.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&resolved)?;
    Ok(resolved)
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

#[cfg(test)]
mod tests {
    use super::*;

    // Regression for the symptom reported after PR #151:
    //   clud  (no args, interactive terminal)
    //   → [clud] daemon session sess-...
    //   → Error: Input must be provided either through stdin or as a
    //     prompt argument when using --print
    //
    // Cause: the centralized daemon mapped `LaunchMode::Subprocess`
    // straight through to `SessionKind::Subprocess`, and the worker's
    // subprocess path uses `StdinMode::Null`. Claude saw no TTY,
    // dropped into `--print` mode, and bailed for lack of a prompt.
    // Interactive launches must force PTY so the worker hands the
    // backend a pseudo-terminal it can drive.
    #[test]
    fn interactive_launch_forces_pty_even_when_plan_says_subprocess() {
        assert!(matches!(
            select_session_kind(LaunchMode::Subprocess, false, false),
            SessionKind::Pty
        ));
    }

    #[test]
    fn interactive_pty_plan_stays_pty() {
        assert!(matches!(
            select_session_kind(LaunchMode::Pty, false, false),
            SessionKind::Pty
        ));
    }

    #[test]
    fn prompted_subprocess_plan_stays_subprocess() {
        // `clud -p "hi"` — claude consumes the prompt arg, no TTY needed.
        assert!(matches!(
            select_session_kind(LaunchMode::Subprocess, false, true),
            SessionKind::Subprocess
        ));
    }

    #[test]
    fn prompted_pty_plan_stays_pty() {
        assert!(matches!(
            select_session_kind(LaunchMode::Pty, false, true),
            SessionKind::Pty
        ));
    }

    #[test]
    fn repeat_jobs_always_subprocess() {
        // Repeat jobs are background, have their own embedded prompt,
        // and never need an attached TTY — even for the interactive
        // case the override must win.
        assert!(matches!(
            select_session_kind(LaunchMode::Subprocess, true, false),
            SessionKind::Subprocess
        ));
        assert!(matches!(
            select_session_kind(LaunchMode::Pty, true, false),
            SessionKind::Subprocess
        ));
        assert!(matches!(
            select_session_kind(LaunchMode::Subprocess, true, true),
            SessionKind::Subprocess
        ));
    }

    #[test]
    fn transcript_forces_centralized_daemon() {
        let args = Args {
            prompt: Some("hi".into()),
            message: None,
            continue_session: false,
            resume: None,
            claude: false,
            codex: false,
            subprocess: false,
            pty: false,
            graphics: crate::graphics::GraphicsMode::Auto,
            graphics_image: None,
            demo_gfx_sixel: false,
            model: None,
            safe: false,
            dry_run: false,
            detach: false,
            detachable: false,
            session_name: None,
            transcript: Some(PathBuf::from("session.log")),
            backlog_size: None,
            verbose: false,
            no_dnd: false,
            clean_worktrees: false,
            fix_hooks: false,
            no_fix_hooks: false,
            stale_after: "1d".into(),
            yes: false,
            force: false,
            experimental_daemon_centralized: false,
            daemon_state_dir: None,
            daemon_mode: None,
            no_daemon: false,
            command: None,
            passthrough: Vec::new(),
        };
        assert!(experimental_enabled(&args));
    }
}
