//! Shared helpers for `tests/pty_behavior.rs` and `tests/pty_pump.rs`.
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

use running_process_core::pty::NativePtyProcess;
use running_process_core::{
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

    let candidates: Vec<PathBuf> = if cfg!(windows) {
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
            running_process_core::pty::poll_pty_process(&process.handles, &process.returncode)
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
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let argv: Vec<String> = if cfg!(windows) {
            vec!["cmd.exe".into(), "/c".into(), "echo clud_canary".into()]
        } else {
            vec!["/bin/sh".into(), "-c".into(), "echo clud_canary".into()]
        };
        let Ok(process) = NativePtyProcess::new(argv, None, None, 24, 80, None) else {
            return false;
        };
        process.set_echo(false);
        if process.start_impl().is_err() {
            return false;
        }
        let buf = drain_reader(&process, Duration::from_secs(3));
        let _ = process.wait_impl(Some(2.0));
        let _ = process.close_impl();
        String::from_utf8_lossy(&buf).contains("clud_canary")
    })
}

/// Skip the current test when the PTY subsystem isn't reliably relaying
/// output in this host environment (typically: nested Windows shells where
/// the parent stdout is a pipe, so ConPTY can't attach a real console).
/// Leaves a diagnostic on stderr so CI logs show the reason.
#[macro_export]
macro_rules! require_pty_or_skip {
    ($test_name:literal) => {
        if !$crate::common::pty_canary() {
            eprintln!(
                "[{}] SKIP: PTY canary failed in this host environment (parent stdout is not a real console).",
                $test_name
            );
            return;
        }
    };
}
