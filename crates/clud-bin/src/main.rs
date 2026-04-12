mod args;
mod backend;
mod command;

use std::io::{self, Read};
use std::process;

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
                // In dry-run mode, use the executable name even if not found.
                backend.executable_name().to_string()
            } else {
                eprintln!(
                    "error: {} not found on PATH. Install it or use --dry-run.",
                    backend.executable_name()
                );
                process::exit(1);
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
        process::exit(0);
    }

    // Execute the command
    let exit_code = run_plan(&plan, args.verbose);
    process::exit(exit_code);
}

fn run_plan(plan: &command::LaunchPlan, verbose: bool) -> i32 {
    let mut last_exit = 0i32;

    for iteration in 0..plan.iterations {
        if plan.iterations > 1 {
            eprintln!("[clud] iteration {}/{}", iteration + 1, plan.iterations);
        }

        if verbose {
            eprintln!("[clud] exec: {}", plan.command.join(" "));
        }

        let status = process::Command::new(&plan.command[0])
            .args(&plan.command[1..])
            .status();

        match status {
            Ok(s) => {
                last_exit = s.code().unwrap_or(1);
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
                eprintln!("[clud] failed to execute {}: {}", plan.command[0], e);
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
