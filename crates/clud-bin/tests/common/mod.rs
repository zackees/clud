//! Shared helpers for `tests/pty/pty_behavior.rs` and `tests/pty/pty_pump.rs`.
//!
//! Cargo treats files under `tests/` as separate integration-test crates,
//! but `tests/common/mod.rs` is brought in by each via `mod common;` and
//! is *not* itself compiled as a test binary. Helpers live here so the
//! two test files stay independently focused below the 1K-LOC ceiling.

// Each test crate uses only a subset of these helpers; suppress the
// resulting unused-code warnings rather than per-symbol `#[allow]`s.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use running_process::pty::NativePtyProcess;
use running_process::{
    CommandSpec, NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode,
};
use serde_json::Value;

/// Locate (and if necessary build) the workspace `mock-agent` binary.
///
/// Uses `CARGO_MANIFEST_DIR` / `CARGO_TARGET_DIR` and prefers the freshest
/// of the plausible target-triple-qualified paths — soldr / ci.env on
/// Windows build into `target/x86_64-pc-windows-msvc/debug/`, plain cargo
/// builds into `target/debug/`. Picking the freshest avoids serving a stale
/// pre-change binary to tests.
pub fn mock_agent_path() -> PathBuf {
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let file_name = format!("mock-agent{}", ext);

    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            manifest
                .parent()
                .and_then(|p| p.parent())
                .map(|p| p.join("target"))
                .expect("workspace target dir")
        });

    // Known triples across the 6 CI targets plus the default debug path.
    let by_triple = |triple: &str| target_dir.join(triple).join("debug").join(&file_name);
    let default = target_dir.join("debug").join(&file_name);

    let mut candidates: Vec<PathBuf> = if cfg!(windows) {
        vec![
            by_triple("x86_64-pc-windows-msvc"),
            by_triple("aarch64-pc-windows-msvc"),
            default.clone(),
        ]
    } else if cfg!(target_os = "macos") {
        vec![
            by_triple("aarch64-apple-darwin"),
            by_triple("x86_64-apple-darwin"),
            default.clone(),
        ]
    } else {
        vec![
            by_triple("x86_64-unknown-linux-gnu"),
            by_triple("aarch64-unknown-linux-gnu"),
            default.clone(),
        ]
    };

    if let Ok(exe) = std::env::current_exe() {
        if let Some(debug_dir) = exe.parent().and_then(|path| path.parent()) {
            candidates.push(debug_dir.join(&file_name));
        }
    }

    let freshest = candidates
        .iter()
        .filter(|p| p.is_file())
        .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    if let Some(path) = freshest {
        return path.clone();
    }

    // Fall back: ask Cargo to build `mock-agent` and report the exact
    // executable path it produced, instead of guessing target-dir layouts.
    let cargo_exe: String = std::env::var_os("CARGO")
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_else(|| "cargo".into());
    let config = ProcessConfig {
        command: CommandSpec::Argv(vec![
            cargo_exe,
            "build".into(),
            "-p".into(),
            "mock-agent".into(),
            "--message-format".into(),
            "json".into(),
        ]),
        cwd: None,
        env: None,
        capture: true,
        stderr_mode: StderrMode::Stdout,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
    };
    let process = NativeProcess::new(config);
    process.start().expect("spawn cargo build -p mock-agent");
    let mut output = String::new();
    let code = loop {
        match process.read_combined(Some(Duration::from_millis(50))) {
            ReadStatus::Line(event) => {
                output.push_str(&String::from_utf8_lossy(&event.line));
                output.push('\n');
            }
            ReadStatus::Timeout | ReadStatus::Eof => {}
        }
        match process.poll() {
            Ok(Some(code)) => break code,
            Ok(None) => {}
            Err(err) => panic!("cargo build -p mock-agent poll failed: {}", err),
        }
    };
    assert_eq!(code, 0, "cargo build -p mock-agent exited with {}", code);

    if let Some(path) = cargo_built_executable_path(&output) {
        return path;
    }

    candidates
        .iter()
        .filter(|p| p.is_file())
        .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .cloned()
        .expect("mock-agent binary not found after build")
}

pub fn cargo_built_executable_path(output: &str) -> Option<PathBuf> {
    output.lines().find_map(|line| {
        let value: Value = serde_json::from_str(line).ok()?;
        let reason = value.get("reason")?.as_str()?;
        if reason != "compiler-artifact" {
            return None;
        }
        let target = value.get("target")?;
        let name = target.get("name")?.as_str()?;
        let kind = target.get("kind")?.as_array()?;
        let is_bin = kind.iter().any(|entry| entry.as_str() == Some("bin"));
        if name != "mock-agent" || !is_bin {
            return None;
        }
        value
            .get("executable")
            .and_then(|entry| entry.as_str())
            .map(PathBuf::from)
            .filter(|path| path.is_file())
    })
}

/// Wait up to `timeout` for `f` to return `true`, sleeping 50ms between polls.
pub fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Wait until mock-agent's `--mock-ready-file` exists: its stdin mode is
/// final and bytes sent from now on arrive intact (#1310). On Windows,
/// ConPTY converts input to key records under the console mode current when
/// the bytes arrive, so input sent before the child switches to VT mode loses
/// its escape sequences. Panics after 20 s so a child that never starts fails
/// loudly instead of hanging.
pub fn wait_for_mock_ready(ready_file: &std::path::Path) {
    assert!(
        wait_until(Duration::from_secs(20), || ready_file.exists()),
        "mock-agent never signalled ready at {}",
        ready_file.display()
    );
}

/// Drain all chunks from the PTY reader up to `overall_timeout` or child exit.
pub fn drain_reader(process: &NativePtyProcess, overall_timeout: Duration) -> Vec<u8> {
    let deadline = Instant::now() + overall_timeout;
    let mut buf = Vec::new();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let slice = remaining.as_secs_f64().min(0.2);
        match process.read_chunk_impl(Some(slice)) {
            Ok(Some(chunk)) => buf.extend_from_slice(&chunk),
            Ok(None) => {}
            Err(_) => break,
        }
        if let Ok(Some(_)) =
            running_process::pty::poll_pty_process(&process.handles, &process.returncode)
        {
            while let Ok(Some(chunk)) = process.read_chunk_impl(Some(0.1)) {
                buf.extend_from_slice(&chunk);
            }
            break;
        }
    }
    buf
}

/// One-shot probe: spawn a trivial command in a PTY and check that its
/// stdout actually reaches us. On Windows ConPTY, this fails when the host
/// process's stdout is redirected (nested shells, captured cargo test).
/// We cache the result so the probe only runs once per test binary.
pub fn pty_canary() -> bool {
    canary_result().is_ok()
}

/// What the canary saw when it failed, for the `CLUD_REQUIRE_PTY` panic.
pub fn pty_canary_diagnostic() -> String {
    match canary_result() {
        Ok(()) => "canary passed".to_string(),
        Err(detail) => detail.clone(),
    }
}

fn canary_result() -> &'static Result<(), String> {
    static CACHED: OnceLock<Result<(), String>> = OnceLock::new();
    CACHED.get_or_init(|| {
        let argv: Vec<String> = if cfg!(windows) {
            vec!["cmd.exe".into(), "/c".into(), "echo clud_canary".into()]
        } else {
            vec!["/bin/sh".into(), "-c".into(), "echo clud_canary".into()]
        };
        let process = NativePtyProcess::new(argv, None, None, 24, 80, None)
            .map_err(|err| format!("spawn failed: {err}"))?;
        process.set_echo(false);
        process
            .start_impl()
            .map_err(|err| format!("start failed: {err}"))?;
        let buf = drain_answering_cursor_queries(&process, Duration::from_secs(5));
        let _ = process.wait_impl(Some(2.0));
        let _ = process.close_impl();
        if String::from_utf8_lossy(&buf).contains("clud_canary") {
            Ok(())
        } else {
            Err(format!(
                "received {} bytes: {:?}",
                buf.len(),
                String::from_utf8_lossy(&buf)
            ))
        }
    })
}

/// Like [`drain_reader`], but answers each `ESC [ 6 n` cursor-position query
/// with `ESC [ 1 ; 1 R`. ConPTY sends that query when it starts and can hold
/// back the child's output until a terminal replies. A real terminal answers
/// it through clud's pump; a bare test harness has nothing to answer it.
pub fn drain_answering_cursor_queries(
    process: &NativePtyProcess,
    overall_timeout: Duration,
) -> Vec<u8> {
    const DSR: &[u8] = b"\x1b[6n";
    let deadline = Instant::now() + overall_timeout;
    let mut buf = Vec::new();
    let mut answered = 0;
    while Instant::now() < deadline {
        match process.read_chunk_impl(Some(0.2)) {
            Ok(Some(chunk)) => buf.extend_from_slice(&chunk),
            Ok(None) => {}
            Err(_) => break,
        }
        let queries = buf.windows(DSR.len()).filter(|w| *w == DSR).count();
        while answered < queries {
            let _ = process.write_impl(b"\x1b[1;1R", false);
            answered += 1;
        }
        if String::from_utf8_lossy(&buf).contains("clud_canary") {
            break;
        }
    }
    buf
}

/// Environment variable that turns a failed PTY canary from a skip into a
/// test failure. CI sets it on the harness it runs inside a pseudo-terminal
/// (`ci/run_bundle.py`), so the configuration interactive launches ship —
/// clud under a real terminal — cannot silently lose its coverage (#691).
pub const REQUIRE_PTY_ENV: &str = "CLUD_REQUIRE_PTY";

/// Whether `CLUD_REQUIRE_PTY` asks for a hard failure: set and not
/// `0`/`false`/empty.
pub fn pty_required() -> bool {
    std::env::var(REQUIRE_PTY_ENV).is_ok_and(|value| {
        let value = value.trim();
        !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
    })
}

/// Skip the current test when the PTY subsystem isn't reliably relaying
/// output in this host environment (typically: nested Windows shells where
/// the parent stdout is a pipe, so ConPTY can't attach a real console).
/// Leaves a diagnostic on stderr so CI logs show the reason. Under
/// `CLUD_REQUIRE_PTY=1` the canary failure panics instead.
#[macro_export]
macro_rules! require_pty_or_skip {
    ($test_name:literal) => {
        if !$crate::common::pty_canary() {
            if $crate::common::pty_required() {
                panic!(
                    "[{}] PTY canary failed and {}=1 requires a working PTY: {}",
                    $test_name,
                    $crate::common::REQUIRE_PTY_ENV,
                    $crate::common::pty_canary_diagnostic()
                );
            }
            eprintln!(
                "[{}] SKIP: PTY canary failed in this host environment (parent stdout is not a real console).",
                $test_name
            );
            return;
        }
    };
}
