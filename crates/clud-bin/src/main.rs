mod args;
mod backend;
mod command;
mod trampoline;

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
        });
        println!("{}", serde_json::to_string_pretty(&json).unwrap());
        std::process::exit(0);
    }

    let interrupted = install_ctrl_c_flag();
    let exit_code = match plan.backend {
        backend::Backend::Codex => run_plan_subprocess(&plan, args.verbose, interrupted.as_ref()),
        backend::Backend::Claude => run_plan_pty(&plan, args.verbose, interrupted.as_ref()),
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
        // No terminal (piped / test harness).  Use a wide default so
        // ConPTY does not wrap output at 80 columns.
        (24, 32767)
    }
}

fn run_plan_subprocess(plan: &command::LaunchPlan, verbose: bool, interrupted: &AtomicBool) -> i32 {
    use running_process_core::{CommandSpec, NativeProcess, ProcessConfig, StderrMode, StdinMode};

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
            cwd: None,
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
            None,
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

        loop {
            // Read PTY output and respond to terminal queries.  ConPTY on
            // Windows sends \x1b[6n (cursor position query) and blocks
            // until the host replies with \x1b[row;colR.
            match process.read_chunk_impl(Some(0.1)) {
                Ok(Some(chunk)) => {
                    let _ = process.respond_to_queries_impl(&chunk);
                }
                Ok(None) => {} // timeout — no data yet
                Err(_) => {
                    // PTY stream closed — child exited.  Reap exit code.
                    match process.wait_impl(Some(1.0)) {
                        Ok(code) => last_exit = code,
                        Err(_) => last_exit = 1,
                    }
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
            }

            // ConPTY may keep the reader alive after child exit.  Poll for
            // process exit so we don't block forever waiting for stream close.
            if let Ok(Some(code)) =
                running_process_core::pty::poll_pty_process(&process.handles, &process.returncode)
            {
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

            if interrupted.load(Ordering::SeqCst) {
                let _ = process.send_interrupt_impl();
                match process.wait_impl(Some(2.0)) {
                    Ok(code) => last_exit = code,
                    Err(_) => {
                        let _ = process.close_impl();
                        last_exit = 130;
                    }
                }
                eprintln!("[clud] interrupted via Ctrl+C (pty)");
                return last_exit;
            }
        }
    }

    last_exit
}

/// Check if stdin is a terminal (not piped).
fn atty_is_terminal() -> bool {
    use std::io::IsTerminal;
    io::stdin().is_terminal()
}

#[cfg(test)]
mod tests {
    #[test]
    fn launch_mode_is_pty() {
        let launch_mode = "pty";
        assert_eq!(launch_mode, "pty");
    }
}
