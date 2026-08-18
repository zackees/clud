//! Mock agent binary for integration testing.
//!
//! This binary is copied/symlinked as `claude` or `codex` in a temp directory
//! and placed on PATH. It records the args it received and exits.
//!
//! Behavior:
//! - Writes received args as JSON to stdout
//! - Reads stdin if available (for pipe mode testing)
//! - Exits with the code specified by --mock-exit-code (default 0)
//! - With a leading `--version` arg, prints `MOCK_CLAUDE_VERSION`
//!   (default `9.9.9 (mock-agent)`) so tests can exercise clud's Claude Code
//!   version gate
//! - With --mock-read-stdin-ms, reads stdin for N ms (even if terminal) and reports it
//! - With --mock-stdin-raw-to, writes captured stdin bytes (pre-JSON) to a file
//!   using Rust byte-literal escaping (e.g., `\x1b`) so binary input is preserved.
//! - With --mock-report-pty-size, polls and reports host/PTY dimensions via the
//!   `terminal_size` crate to a JSON file for the resize-propagation test.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CODEX_BRIDGE_PROBE_REQUEST: &str = include_str!("../assets/codex_bridge_probe_request.json");

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
    let mut ansi_script: Option<PathBuf> = None;
    let mut tool_shell_probe_to: Option<PathBuf> = None;
    let mut codex_bridge_probe_to: Option<PathBuf> = None;
    // Emit canned `--output-format stream-json` lines from a file (one line
    // each, separated by `--mock-stream-delay-ms`). Used by integration tests
    // that exercise clud's stream-json renderer without needing a real
    // claude subscription key.
    let mut stream_json_script: Option<PathBuf> = None;
    let mut stream_delay_ms: u64 = 0;
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
        if arg == "--mock-ansi-script" {
            if let Some(path) = args.get(i + 1) {
                ansi_script = Some(PathBuf::from(path));
            }
            skip_next = true;
            continue;
        }
        if arg == "--mock-tool-shell-probe" {
            if let Some(path) = args.get(i + 1) {
                tool_shell_probe_to = Some(PathBuf::from(path));
            }
            skip_next = true;
            continue;
        }
        if arg == "--mock-codex-bridge-probe" {
            if let Some(path) = args.get(i + 1) {
                codex_bridge_probe_to = Some(PathBuf::from(path));
            }
            skip_next = true;
            continue;
        }
        if arg == "--mock-stream-json" {
            if let Some(path) = args.get(i + 1) {
                stream_json_script = Some(PathBuf::from(path));
            }
            skip_next = true;
            continue;
        }
        if arg == "--mock-stream-delay-ms" {
            if let Some(ms) = args.get(i + 1) {
                stream_delay_ms = ms.parse().unwrap_or(0);
            }
            skip_next = true;
            continue;
        }
        filtered_args.push(arg.clone());
    }

    // `clud` probes `claude --version` for the unified gateway-discovery
    // floor (issue #921). Stand in for a real Claude Code install: print the
    // version string from `MOCK_CLAUDE_VERSION` (default: a release far above
    // the floor, so existing tests stay silent) and exit before the JSON
    // report machinery runs.
    if args.get(1).map(String::as_str) == Some("--version") {
        let version = std::env::var("MOCK_CLAUDE_VERSION")
            .unwrap_or_else(|_| "9.9.9 (mock-agent)".to_string());
        println!("{version}");
        return;
    }

    // Emit scripted ANSI bytes before everything else so they land in the
    // PTY / terminal capture first. Used by the attach-replay integration
    // test to paint a known frame whose presence we can verify in a
    // post-detach reattach's snapshot.
    if let Some(path) = ansi_script.as_ref() {
        if let Ok(bytes) = std::fs::read(path) {
            let _ = io::stdout().write_all(&bytes);
            let _ = io::stdout().flush();
        }
    }

    // Emit canned stream-json events line-by-line. When the integration
    // test wants to exercise clud's renderer, it points this at a file
    // containing one JSON event per line; we write them with a flush
    // between each so the parent's drain loop sees them incrementally,
    // exactly like real claude would emit them. Then exit immediately
    // so the test's wait completes without the usual JSON-report tail.
    if let Some(path) = stream_json_script.as_ref() {
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                let _ = io::stdout().write_all(line.as_bytes());
                let _ = io::stdout().write_all(b"\n");
                let _ = io::stdout().flush();
                if stream_delay_ms > 0 {
                    std::thread::sleep(Duration::from_millis(stream_delay_ms));
                }
            }
        }
        std::process::exit(exit_code);
    }

    if let Some(path) = pty_size_report_to.as_ref() {
        run_pty_size_probe(path, pty_size_samples.max(1), pty_size_interval_ms);
        std::process::exit(exit_code);
    }

    if let Some(role) = helper_role.as_deref() {
        run_helper(&args[0], role, tree_log.as_ref(), sleep_ms);
        return;
    }

    if let Some(path) = tool_shell_probe_to.as_ref() {
        run_tool_shell_probe(&args[0], path);
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
    let anthropic_base_url = std::env::var("ANTHROPIC_BASE_URL").ok();
    let anthropic_auth_token = std::env::var("ANTHROPIC_AUTH_TOKEN").ok();
    let anthropic_base_url_present = anthropic_base_url.is_some();
    let anthropic_auth_token_present = anthropic_auth_token.is_some();
    let anthropic_api_key_present = std::env::var_os("ANTHROPIC_API_KEY").is_some();
    let api_timeout_ms = std::env::var("API_TIMEOUT_MS").ok();
    let disable_nonessential_traffic =
        std::env::var("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC").ok();
    let enable_gateway_model_discovery =
        std::env::var("CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY").ok();
    let max_context_tokens = std::env::var("CLAUDE_CODE_MAX_CONTEXT_TOKENS").ok();
    let bridge_probe = codex_bridge_probe_to.as_deref().map(run_codex_bridge_probe);
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
            "ANTHROPIC_BASE_URL_PRESENT": anthropic_base_url_present,
            "ANTHROPIC_AUTH_TOKEN_PRESENT": anthropic_auth_token_present,
            "ANTHROPIC_API_KEY_PRESENT": anthropic_api_key_present,
            "API_TIMEOUT_MS": api_timeout_ms,
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": disable_nonessential_traffic,
            "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY": enable_gateway_model_discovery,
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS": max_context_tokens,
        },
        "codex_bridge_probe": bridge_probe,
    });

    let report_str = serde_json::to_string(&report).unwrap();
    if [
        anthropic_base_url.as_deref(),
        anthropic_auth_token.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|secret| !secret.is_empty() && report_str.contains(secret))
    {
        eprintln!("mock-agent refused to serialize a bridge credential");
        std::process::exit(86);
    }
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

fn run_codex_bridge_probe(report_path: &Path) -> serde_json::Value {
    let base_url = std::env::var("ANTHROPIC_BASE_URL").ok();
    let token = std::env::var("ANTHROPIC_AUTH_TOKEN").ok();
    let mut report = serde_json::json!({
        "attempted": false,
        "loopback": false,
        "port": null,
        "status": null,
        "bridged_reply_received": false,
        "error": null,
    });

    let result = (|| -> Result<(), String> {
        let base_url = base_url.as_deref().ok_or("missing bridge URL")?;
        let token = token.as_deref().ok_or("missing bridge token")?;
        let address: SocketAddr = base_url
            .strip_prefix("http://")
            .ok_or("bridge URL is not HTTP")?
            .parse()
            .map_err(|_| "bridge URL is not a socket address")?;
        report["attempted"] = true.into();
        report["loopback"] = address.ip().is_loopback().into();
        report["port"] = address.port().into();

        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
            .map_err(|error| format!("connect failed: {error}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|error| format!("read timeout failed: {error}"))?;
        let body = CODEX_BRIDGE_PROBE_REQUEST.trim();
        let request = format!(
            "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|error| format!("write failed: {error}"))?;
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .map_err(|error| format!("read failed: {error}"))?;
        let status = response
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse::<u16>().ok())
            .ok_or("missing HTTP status")?;
        report["status"] = status.into();
        // #627 step 5: the bridge now translates a real upstream reply, so the
        // probe looks for the text the fake Responses server produced.
        report["bridged_reply_received"] = response.contains("bridged reply").into();
        Ok(())
    })();
    if let Err(error) = result {
        report["error"] = error.into();
    }
    if let Some(parent) = report_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(report_path, serde_json::to_vec(&report).unwrap_or_default());
    report
}

/// Put stdin into raw mode (no canonical line buffering, no echo) when it is
/// a terminal. No-op on non-Unix or when stdin is a pipe.
#[cfg(unix)]
fn set_stdin_raw_if_tty() {
    use std::os::unix::io::AsRawFd;
    let fd = io::stdin().as_raw_fd();
    if unsafe { libc::isatty(fd) } == 0 {
        return;
    }
    let mut termios: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut termios) } != 0 {
        return;
    }
    unsafe { libc::cfmakeraw(&mut termios) };
    let _ = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios) };
}

#[cfg(not(unix))]
fn set_stdin_raw_if_tty() {}

/// Read from stdin for up to `timeout_ms` milliseconds, collecting whatever arrives.
/// Works regardless of whether stdin is a terminal or pipe.
fn read_stdin_timed(timeout_ms: u64) -> Option<Vec<u8>> {
    // Real TUI children (e.g., codex Ink) put their PTY slave into raw mode
    // before reading. The mock-agent must do the same when its stdin is a PTY
    // slave, otherwise the kernel's canonical line discipline holds non-
    // newline-terminated bytes (like the F3 voice-mode transcript) forever
    // and they never reach the test's stdin capture.
    set_stdin_raw_if_tty();

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

#[cfg(windows)]
fn run_tool_shell_probe(exe: &str, report_path: &Path) {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    let pid_path = report_path.with_extension("pid");
    let quote = |value: &str| format!("'{}'", value.replace('\'', "''"));
    let script = format!(
        "$p = Start-Process -FilePath {} \
         -ArgumentList '--mock-helper-role','tool-leak','--mock-sleep-ms','30000' -PassThru; \
         Set-Content -LiteralPath {} -Value $p.Id",
        quote(exe),
        quote(&pid_path.to_string_lossy()),
    );
    let status = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &script,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn tool PowerShell");
    assert!(status.success(), "tool PowerShell failed: {status}");

    let pid: u32 = std::fs::read_to_string(&pid_path)
        .expect("read leaked-client pid")
        .trim()
        .parse()
        .expect("parse leaked-client pid");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut system = System::new();
    let reaped = loop {
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
            true,
            ProcessRefreshKind::nothing(),
        );
        if system.process(Pid::from_u32(pid)).is_none() {
            break true;
        }
        if Instant::now() >= deadline {
            if let Some(process) = system.process(Pid::from_u32(pid)) {
                let _ = process.kill();
            }
            break false;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let report = serde_json::json!({
        "tool_shell_probe": {
            "client_pid": pid,
            "reaped": reaped,
        }
    });
    if let Some(parent) = report_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(report_path, report.to_string()).expect("write tool-shell probe report");
    println!("{report}");
    std::process::exit(if reaped { 0 } else { 1 });
}

#[cfg(not(windows))]
fn run_tool_shell_probe(_exe: &str, report_path: &Path) {
    let report = serde_json::json!({
        "tool_shell_probe": {
            "unsupported": true,
        }
    });
    std::fs::write(report_path, report.to_string()).expect("write tool-shell probe report");
    println!("{report}");
}

use std::io::IsTerminal;
