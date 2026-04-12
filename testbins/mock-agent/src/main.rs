//! Mock agent binary for integration testing.
//!
//! This binary is copied/symlinked as `claude` or `codex` in a temp directory
//! and placed on PATH. It records the args it received and exits.
//!
//! Behavior:
//! - Writes received args as JSON to stdout
//! - Reads stdin if available (for pipe mode testing)
//! - Exits with the code specified by --mock-exit-code (default 0)

use std::io::{self, Read};

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Extract --mock-exit-code if present (our own flag, not forwarded by clud)
    let mut exit_code = 0i32;
    let mut filtered_args: Vec<String> = Vec::new();
    let mut skip_next = false;
    for (i, arg) in args.iter().enumerate().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--mock-exit-code" {
            if let Some(code) = args.get(i + 1) {
                exit_code = code.parse().unwrap_or(0);
            }
            skip_next = true;
            continue;
        }
        filtered_args.push(arg.clone());
    }

    // Read stdin if not a terminal
    let stdin_content = if !io::stdin().is_terminal() {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf).ok();
        if buf.is_empty() {
            None
        } else {
            Some(buf)
        }
    } else {
        None
    };

    // Output JSON report of what we received
    let report = serde_json::json!({
        "program": args[0],
        "args": filtered_args,
        "stdin": stdin_content,
        "exit_code": exit_code,
    });
    println!("{}", serde_json::to_string(&report).unwrap());

    std::process::exit(exit_code);
}

use std::io::IsTerminal;
