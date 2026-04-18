//! Mock agent binary for integration testing.
//!
//! This binary is copied/symlinked as `claude` or `codex` in a temp directory
//! and placed on PATH. It records the args it received and exits.
//!
//! Behavior:
//! - Writes received args as JSON to stdout
//! - Reads stdin if available (for pipe mode testing)
//! - Exits with the code specified by --mock-exit-code (default 0)
//! - With --mock-read-stdin-ms, reads stdin for N ms (even if terminal) and reports it
//! - With --mock-stdin-raw-to, writes captured stdin bytes (pre-JSON) to a file
//!   using Rust byte-literal escaping (e.g., `\x1b`) so binary input is preserved.
//! - With --mock-report-pty-size, polls and reports host/PTY dimensions via the
//!   `terminal_size` crate to a JSON file for the resize-propagation test.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Extract --mock-exit-code if present (our own flag, not forwarded by clud)
    let mut exit_code = 0i32;
    let mut sleep_ms = 0u64;
    let mut read_stdin_ms = 0u64;
    let mut helper_role: Option<String> = None;
    let mut tree_log: Option<PathBuf> = None;
    let mut report_file: Option<PathBuf> = None;
    let mut write_done_at: Option<PathBuf> = None;
    let mut write_done_body = String::from("mock-done");
    let mut write_blocked_at: Option<PathBuf> = None;
    let mut write_blocked_body = String::from("mock-blocked");
    let mut write_marker_on_iter: u32 = 0;
    let mut stdin_raw_to: Option<PathBuf> = None;
    let mut pty_size_report_to: Option<PathBuf> = None;
    let mut pty_size_samples: u32 = 0;
    let mut pty_size_interval_ms: u64 = 100;
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
        if arg == "--mock-write-done" {
            if let Some(path) = args.get(i + 1) {
                write_done_at = Some(PathBuf::from(path));
            }
            skip_next = true;
            continue;
        }
        if arg == "--mock-write-done-body" {
            if let Some(body) = args.get(i + 1) {
                write_done_body = body.clone();
            }
            skip_next = true;
            continue;
        }
        if arg == "--mock-write-blocked" {
            if let Some(path) = args.get(i + 1) {
                write_blocked_at = Some(PathBuf::from(path));
            }
            skip_next = true;
            continue;
        }
        if arg == "--mock-write-blocked-body" {
            if let Some(body) = args.get(i + 1) {
                write_blocked_body = body.clone();
            }
            skip_next = true;
            continue;
        }
        if arg == "--mock-write-marker-on-iter" {
            if let Some(n) = args.get(i + 1) {
                write_marker_on_iter = n.parse().unwrap_or(0);
            }
            skip_next = true;
            continue;
        }
        if arg == "--mock-sleep-ms" {
            if let Some(ms) = args.get(i + 1) {
                sleep_ms = ms.parse().unwrap_or(0);
            }
            skip_next = true;
            continue;
        }
        if arg == "--mock-read-stdin-ms" {
            if let Some(ms) = args.get(i + 1) {
                read_stdin_ms = ms.parse().unwrap_or(0);
            }
            skip_next = true;
            continue;
        }
        if arg == "--mock-helper-role" {
            if let Some(role) = args.get(i + 1) {
                helper_role = Some(role.clone());
            }
            skip_next = true;
            continue;
        }
        if arg == "--mock-spawn-tree-log" {
            if let Some(path) = args.get(i + 1) {
                tree_log = Some(PathBuf::from(path));
            }
            skip_next = true;
            continue;
        }
        if arg == "--mock-report-file" {
            if let Some(path) = args.get(i + 1) {
                report_file = Some(PathBuf::from(path));
            }
            skip_next = true;
            continue;
        }
        if arg == "--mock-stdin-raw-to" {
            if let Some(path) = args.get(i + 1) {
                stdin_raw_to = Some(PathBuf::from(path));
            }
            skip_next = true;
            continue;
        }
        if arg == "--mock-report-pty-size" {
            if let Some(path) = args.get(i + 1) {
                pty_size_report_to = Some(PathBuf::from(path));
            }
            skip_next = true;
            continue;
        }
        if arg == "--mock-pty-size-samples" {
            if let Some(n) = args.get(i + 1) {
                pty_size_samples = n.parse().unwrap_or(0);
            }
            skip_next = true;
            continue;
        }
        if arg == "--mock-pty-size-interval-ms" {
            if let Some(n) = args.get(i + 1) {
                pty_size_interval_ms = n.parse().unwrap_or(100);
            }
            skip_next = true;
            continue;
        }
        filtered_args.push(arg.clone());
    }

    if let Some(path) = pty_size_report_to.as_ref() {
        run_pty_size_probe(path, pty_size_samples.max(1), pty_size_interval_ms);
        std::process::exit(exit_code);
    }

    if let Some(role) = helper_role.as_deref() {
        run_helper(&args[0], role, tree_log.as_ref(), sleep_ms);
        return;
    }

    // Track which iteration we're on by reading/bumping a counter file whose
    // path is shared by all three marker flags. We compute that path as the
    // parent of the first marker path, suffixed with ".iter-count".
    let counter_path = write_done_at
        .as_ref()
        .or(write_blocked_at.as_ref())
        .map(|p| p.with_file_name("iter-count"));
    let iteration = bump_iter_counter(counter_path.as_deref());

    if write_marker_on_iter > 0 && iteration >= write_marker_on_iter {
        if let Some(path) = write_done_at.as_ref() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, &write_done_body);
        }
        if let Some(path) = write_blocked_at.as_ref() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, &write_blocked_body);
        }
    }

    if let Some(path) = tree_log.as_ref() {
        append_tree_log(path, "root");
        spawn_helper(&args[0], "child", path, sleep_ms);
    }

    let stdin_is_terminal = io::stdin().is_terminal();

    // Read stdin: either timed read (--mock-read-stdin-ms) or pipe-mode read
    let stdin_bytes: Option<Vec<u8>> = if read_stdin_ms > 0 {
        read_stdin_timed(read_stdin_ms)
    } else if !stdin_is_terminal {
        let mut buf = Vec::new();
        io::stdin().read_to_end(&mut buf).ok();
        if buf.is_empty() {
            None
        } else {
            Some(buf)
        }
    } else {
        None
    };

    if let (Some(path), Some(bytes)) = (stdin_raw_to.as_ref(), stdin_bytes.as_ref()) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, bytes);
    }

    let stdin_content: Option<String> = stdin_bytes
        .as_ref()
        .map(|b| String::from_utf8_lossy(b).into_owned());

    // Capture env vars relevant for testing
    let in_clud = std::env::var("IN_CLUD").ok();
    let originator = std::env::var("RUNNING_PROCESS_ORIGINATOR").ok();
    let cwd = std::env::current_dir()
        .ok()
        .map(|path| path.to_string_lossy().to_string());

    if sleep_ms > 0 {
        std::thread::sleep(Duration::from_millis(sleep_ms));
    }

    // Output JSON report of what we received
    let report = serde_json::json!({
        "program": args[0],
        "args": filtered_args,
        "cwd": cwd,
        "stdin": stdin_content,
        "stdin_is_terminal": stdin_is_terminal,
        "exit_code": exit_code,
        "sleep_ms": sleep_ms,
        "env": {
            "IN_CLUD": in_clud,
            "RUNNING_PROCESS_ORIGINATOR": originator,
        },
    });

    let report_str = serde_json::to_string(&report).unwrap();
    println!("{}", report_str);

    // Also write to file if requested (useful when stdout is captured by PTY)
    if let Some(path) = report_file {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, &report_str);
    }

    std::process::exit(exit_code);
}

/// Read from stdin for up to `timeout_ms` milliseconds, collecting whatever arrives.
/// Works regardless of whether stdin is a terminal or pipe.
fn read_stdin_timed(timeout_ms: u64) -> Option<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let stdin = io::stdin();
        loop {
            match stdin.lock().read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut collected = Vec::new();
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(data) => collected.extend(data),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    if collected.is_empty() {
        None
    } else {
        Some(collected)
    }
}

/// Poll `terminal_size::terminal_size()` `samples` times, sleeping
/// `interval_ms` between each poll, and write a JSON array of `(cols, rows)`
/// pairs (nullable when no terminal is detected) to `path`. Also emits a
/// marker line to stdout after each sample so the test harness can drive
/// a mid-run resize between samples.
fn run_pty_size_probe(path: &Path, samples: u32, interval_ms: u64) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut out = Vec::new();
    for i in 0..samples {
        let size = terminal_size::terminal_size();
        let entry = match size {
            Some((w, h)) => serde_json::json!({ "cols": w.0, "rows": h.0 }),
            None => serde_json::json!({ "cols": null, "rows": null }),
        };
        out.push(entry.clone());
        let _ = std::fs::write(path, serde_json::to_string(&out).unwrap());
        let line = format!("PTY_SIZE_SAMPLE {} {}\n", i + 1, entry);
        let _ = io::stdout().write_all(line.as_bytes());
        let _ = io::stdout().flush();
        if i + 1 < samples {
            std::thread::sleep(Duration::from_millis(interval_ms));
        }
    }
}

fn bump_iter_counter(path: Option<&Path>) -> u32 {
    let path = match path {
        Some(p) => p,
        None => return 1,
    };
    let cur: u32 = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let next = cur + 1;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, next.to_string());
    next
}

fn run_helper(exe: &str, role: &str, tree_log: Option<&PathBuf>, sleep_ms: u64) {
    if let Some(path) = tree_log {
        append_tree_log(path, role);
        if role == "child" {
            spawn_helper(exe, "grandchild", path, sleep_ms);
        }
    }
    if sleep_ms > 0 {
        std::thread::sleep(Duration::from_millis(sleep_ms));
    }
}

fn spawn_helper(exe: &str, role: &str, tree_log: &PathBuf, sleep_ms: u64) {
    let mut command = Command::new(exe);
    command
        .arg("--mock-helper-role")
        .arg(role)
        .arg("--mock-spawn-tree-log")
        .arg(tree_log)
        .arg("--mock-sleep-ms")
        .arg(sleep_ms.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _ = command.spawn();
}

fn append_tree_log(path: &PathBuf, role: &str) {
    let parent = path.parent().expect("tree log parent");
    let _ = std::fs::create_dir_all(parent);
    let line = serde_json::json!({
        "role": role,
        "pid": std::process::id(),
        "ppid": std::process::id(),
    });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open tree log");
    use std::io::Write;
    writeln!(file, "{}", line).expect("write tree log");
}

use std::io::IsTerminal;
