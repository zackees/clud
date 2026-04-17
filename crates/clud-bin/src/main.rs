mod args;
mod backend;
mod command;
mod daemon;
mod loop_spec;
mod session;
mod trampoline;
mod voice;
mod wasm;

use std::io::{self, Read};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

fn main() {
    // Windows: rename ourselves so pip can always overwrite clud.exe.
    trampoline::unlock_exe();

    let mut args = args::Args::parse_with_passthrough();

    // Pipe mode: if stdin is not a terminal, read it as the prompt.
    if args.prompt.is_none()
        && args.message.is_none()
        && args.command.is_none()
        && !atty_is_terminal()
    {
        let mut input = String::new();
        if io::stdin().read_to_string(&mut input).is_ok() && !input.trim().is_empty() {
            args.prompt = Some(input.trim().to_string());
        }
    }

    if let Some(args::Command::Wasm { module, invoke }) = &args.command {
        if args.dry_run {
            let json = serde_json::json!({
                "mode": "wasm",
                "module": module,
                "invoke": invoke,
            });
            println!("{}", serde_json::to_string_pretty(&json).unwrap());
            std::process::exit(0);
        }

        match wasm::run_file(module, invoke) {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                eprintln!("error: {error}");
                std::process::exit(1);
            }
        }
    }

    let interrupted = install_ctrl_c_flag();
    if let Some(exit_code) = daemon::handle_special_command(&args, interrupted.as_ref()) {
        std::process::exit(exit_code);
    }

    let backend = backend::resolve_backend(args.claude, args.codex);
    let backend_path = match backend::find_backend(backend) {
        Some(path) => path.to_string_lossy().to_string(),
        None => {
            if args.dry_run {
                backend.executable_name().to_string()
            } else {
                eprintln!(
                    "error: {} not found on PATH. Install it or use --dry-run.",
                    backend.executable_name()
                );
                std::process::exit(1);
            }
        }
    };

    let plan = command::build_launch_plan(&args, backend, &backend_path);

    if args.dry_run {
        let json = serde_json::json!({
            "command": plan.command,
            "iterations": plan.iterations,
            "backend": backend.executable_name(),
            "launch_mode": plan.launch_mode.as_str(),
            "loop_markers": plan.loop_markers.as_ref().map(|m| &m.git_root),
        });
        println!("{}", serde_json::to_string_pretty(&json).unwrap());
        std::process::exit(0);
    }

    // Clear stale DONE/BLOCKED markers from a prior run so that loops don't
    // short-circuit on iteration 1. See loop_spec for semantics.
    if let Some(ref markers) = plan.loop_markers {
        loop_spec::clear_markers(std::path::Path::new(&markers.git_root));
    }

    let exit_code = if daemon::experimental_enabled(&args) {
        daemon::run_centralized_session(&args, &plan, interrupted.as_ref())
    } else {
        match plan.launch_mode {
            backend::LaunchMode::Subprocess => {
                run_plan_subprocess(&plan, args.verbose, interrupted.as_ref())
            }
            backend::LaunchMode::Pty => run_plan_pty(&plan, args.verbose, interrupted.as_ref()),
        }
    };
    std::process::exit(exit_code);
}

/// Build the child environment: inherit parent env + inject tracking vars.
/// Deduplicates keys so we never pass the same var twice.
fn child_env() -> Vec<(String, String)> {
    let originator_key = running_process_core::ORIGINATOR_ENV_VAR;

    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k != "IN_CLUD" && k != originator_key)
        .collect();

    env.push(("IN_CLUD".to_string(), "1".to_string()));

    let originator_value = format!("CLUD:{}", std::process::id());
    env.push((originator_key.to_string(), originator_value));

    env
}

fn install_ctrl_c_flag() -> Arc<AtomicBool> {
    let interrupted = Arc::new(AtomicBool::new(false));
    let handler_flag = Arc::clone(&interrupted);
    if let Err(e) = ctrlc::set_handler(move || {
        handler_flag.store(true, Ordering::SeqCst);
    }) {
        eprintln!("[clud] warning: failed to install Ctrl+C handler: {}", e);
    }
    interrupted
}

fn get_terminal_size() -> (u16, u16) {
    if let Some((w, h)) = terminal_size::terminal_size() {
        (h.0, w.0)
    } else {
        // No terminal (piped / test harness). Use a wide default so
        // ConPTY does not wrap output at 80 columns.
        (24, 32767)
    }
}

fn normalize_exit_code(code: i32) -> i32 {
    match code {
        -2 => 130,
        -9 => 137,
        -15 => 143,
        _ => code,
    }
}

fn run_plan_subprocess(plan: &command::LaunchPlan, verbose: bool, interrupted: &AtomicBool) -> i32 {
    use std::path::PathBuf;

    use running_process_core::{CommandSpec, NativeProcess, ProcessConfig, StderrMode, StdinMode};

    // Enable VT input on the console before launching the child.
    // This allows ANSI sequences (including bracketed paste for drag-and-drop)
    // to flow through to the child process via inherited stdin.
    let _console_guard = enable_console_vt_input();

    let env = child_env();
    let mut last_exit = 0i32;

    for iteration in 0..plan.iterations {
        if plan.iterations > 1 {
            eprintln!("[clud] iteration {}/{}", iteration + 1, plan.iterations);
        }

        if verbose {
            eprintln!("[clud] exec (subprocess): {}", plan.command.join(" "));
        }

        let config = ProcessConfig {
            command: CommandSpec::Argv(plan.command.clone()),
            cwd: plan.cwd.as_ref().map(PathBuf::from),
            env: Some(env.clone()),
            capture: false,
            stderr_mode: StderrMode::Stdout,
            creationflags: None,
            create_process_group: false,
            stdin_mode: StdinMode::Inherit,
            nice: None,
            containment: None,
        };

        let process = NativeProcess::new(config);
        if let Err(e) = process.start() {
            eprintln!("[clud] failed to execute {}: {}", plan.command[0], e);
            return 1;
        }

        loop {
            match process.poll() {
                Ok(Some(code)) => {
                    if interrupted.load(Ordering::SeqCst) {
                        eprintln!("[clud] interrupted via Ctrl+C");
                        return 130;
                    }
                    last_exit = code;
                    if last_exit != 0 && plan.iterations > 1 {
                        eprintln!(
                            "[clud] iteration {} failed with exit code {}",
                            iteration + 1,
                            last_exit
                        );
                        return last_exit;
                    }
                    break;
                }
                Ok(None) => {
                    if interrupted.load(Ordering::SeqCst) {
                        let _ = process.kill();
                        let _ = process.wait(Some(std::time::Duration::from_secs(2)));
                        eprintln!("[clud] interrupted via Ctrl+C");
                        return 130;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => {
                    eprintln!("[clud] error waiting for process: {}", e);
                    return 1;
                }
            }
        }

        if let Some(code) = check_loop_markers(plan, iteration + 1) {
            return code;
        }
    }

    if let Some(code) = loop_unconverged_exit(plan) {
        return code;
    }

    last_exit
}

fn run_plan_pty(plan: &command::LaunchPlan, verbose: bool, interrupted: &AtomicBool) -> i32 {
    use running_process_core::pty::NativePtyProcess;

    let env = child_env();
    let mut last_exit = 0i32;
    let (rows, cols) = get_terminal_size();

    for iteration in 0..plan.iterations {
        if plan.iterations > 1 {
            eprintln!("[clud] iteration {}/{}", iteration + 1, plan.iterations);
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
                return 1;
            }
        };

        process.set_echo(true);

        if let Err(e) = process.start_impl() {
            eprintln!("[clud] failed to execute {}: {}", plan.command[0], e);
            return 1;
        }

        let exit_code = if session::terminals_are_interactive() {
            let mut hooks = voice::VoiceMode::from_env();
            session::run_interactive_pty_session(&process, interrupted, &mut hooks)
        } else {
            session::run_pty_output_loop(&process, interrupted)
        };
        last_exit = normalize_exit_code(exit_code);

        if last_exit != 0 && plan.iterations > 1 {
            eprintln!(
                "[clud] iteration {} failed with exit code {}",
                iteration + 1,
                last_exit
            );
            return last_exit;
        }

        if let Some(code) = check_loop_markers(plan, iteration + 1) {
            return code;
        }
    }

    if let Some(code) = loop_unconverged_exit(plan) {
        return code;
    }

    last_exit
}

/// Check for DONE/BLOCKED markers after an iteration finishes. Returns a
/// terminal exit code to return from the runner, or `None` to continue.
fn check_loop_markers(plan: &command::LaunchPlan, iteration: u32) -> Option<i32> {
    let markers = plan.loop_markers.as_ref()?;
    let root = std::path::Path::new(&markers.git_root);
    match loop_spec::read_markers(root) {
        loop_spec::MarkerState::Done(summary) => {
            if summary.is_empty() {
                eprintln!(
                    "[clud loop] DONE marker detected at iteration {iteration}; task resolved."
                );
            } else {
                eprintln!("[clud loop] DONE at iteration {iteration}: {summary}");
            }
            Some(0)
        }
        loop_spec::MarkerState::Blocked(reason) => {
            if reason.is_empty() {
                eprintln!("[clud loop] BLOCKED marker detected at iteration {iteration}; halting.");
            } else {
                eprintln!("[clud loop] BLOCKED at iteration {iteration}: {reason}");
            }
            Some(3)
        }
        loop_spec::MarkerState::None => None,
    }
}

/// Called after the iteration count is exhausted without a DONE/BLOCKED
/// marker. Only returns an override exit code when loop markers are active.
fn loop_unconverged_exit(plan: &command::LaunchPlan) -> Option<i32> {
    plan.loop_markers.as_ref().map(|_| {
        eprintln!(
            "[clud loop] iteration count ({}) exhausted without a DONE marker; task did not converge.",
            plan.iterations
        );
        2
    })
}

/// RAII guard that restores the original console input mode on drop.
struct ConsoleVtGuard {
    #[cfg(windows)]
    original_mode: Option<u32>,
}

impl Drop for ConsoleVtGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(mode) = self.original_mode {
            restore_console_mode(mode);
        }
    }
}

/// Enable `ENABLE_VIRTUAL_TERMINAL_INPUT` on the Windows console so ANSI
/// sequences (bracketed paste, etc.) pass through to the child process.
/// Returns a guard that restores the original mode on drop.
/// On non-Windows platforms this is a no-op.
fn enable_console_vt_input() -> ConsoleVtGuard {
    #[cfg(windows)]
    {
        use std::io::IsTerminal;
        if !io::stdin().is_terminal() {
            return ConsoleVtGuard {
                original_mode: None,
            };
        }
        match set_console_vt_input(true) {
            Some(original) => ConsoleVtGuard {
                original_mode: Some(original),
            },
            None => ConsoleVtGuard {
                original_mode: None,
            },
        }
    }
    #[cfg(not(windows))]
    {
        ConsoleVtGuard {}
    }
}

#[cfg(windows)]
fn set_console_vt_input(enable: bool) -> Option<u32> {
    use std::os::windows::io::AsRawHandle;

    // Windows console mode flag for virtual terminal input processing.
    const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

    extern "system" {
        fn GetConsoleMode(handle: isize, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: isize, mode: u32) -> i32;
    }

    let handle = io::stdin().as_raw_handle() as isize;
    unsafe {
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return None;
        }
        let original = mode;
        if enable {
            mode |= ENABLE_VIRTUAL_TERMINAL_INPUT;
        } else {
            mode &= !ENABLE_VIRTUAL_TERMINAL_INPUT;
        }
        if SetConsoleMode(handle, mode) == 0 {
            return None;
        }
        Some(original)
    }
}

#[cfg(windows)]
fn restore_console_mode(mode: u32) {
    use std::os::windows::io::AsRawHandle;

    extern "system" {
        fn SetConsoleMode(handle: isize, mode: u32) -> i32;
    }

    let handle = io::stdin().as_raw_handle() as isize;
    unsafe {
        SetConsoleMode(handle, mode);
    }
}

/// Check if stdin is a terminal (not piped).
fn atty_is_terminal() -> bool {
    use std::io::IsTerminal;
    io::stdin().is_terminal()
}

#[cfg(test)]
mod tests {
    #[test]
    fn launch_mode_defaults_to_subprocess() {
        let launch_mode = crate::backend::LaunchMode::Subprocess;
        assert_eq!(launch_mode.as_str(), "subprocess");
    }
}
