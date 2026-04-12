mod args;
mod backend;
mod command;

use std::io::{self, Read};

fn main() {
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

    let exit_code = run_plan(&plan, args.verbose);
    std::process::exit(exit_code);
}

/// Build the child environment: inherit parent env + inject tracking vars.
fn child_env() -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars().collect();

    // Mark that the child is running under clud.
    env.push(("IN_CLUD".to_string(), "1".to_string()));

    // Set originator so running-process can discover orphaned children.
    let originator_value = format!("CLUD:{}", std::process::id());
    env.push((
        running_process_core::ORIGINATOR_ENV_VAR.to_string(),
        originator_value,
    ));

    env
}

fn run_plan(plan: &command::LaunchPlan, verbose: bool) -> i32 {
    use running_process_core::{
        CommandSpec, Containment, NativeProcess, ProcessConfig, StderrMode, StdinMode,
    };

    let env = child_env();
    let mut last_exit = 0i32;

    for iteration in 0..plan.iterations {
        if plan.iterations > 1 {
            eprintln!("[clud] iteration {}/{}", iteration + 1, plan.iterations);
        }

        if verbose {
            eprintln!("[clud] exec: {}", plan.command.join(" "));
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
            containment: Some(Containment::Contained),
        };

        let process = NativeProcess::new(config);
        if let Err(e) = process.start() {
            eprintln!("[clud] failed to execute {}: {}", plan.command[0], e);
            return 1;
        }

        match process.wait(None) {
            Ok(code) => {
                last_exit = code;
                if last_exit != 0 && plan.iterations > 1 {
                    eprintln!(
                        "[clud] iteration {} failed with exit code {}",
                        iteration + 1,
                        last_exit
                    );
                    return last_exit;
                }
            }
            Err(e) => {
                eprintln!("[clud] process error: {}", e);
                return 1;
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
