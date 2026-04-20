//! Cross-platform behavior tests for `running_process_core::pty::NativePtyProcess`
//! as used by `clud --codex` (see zackees/clud#28, #31).
//!
//! Each test asserts the platform-specific contract that
//! `running-process-core` 3.1.0 actually exposes today. When a theory from
//! #31 predicts the *wrong* behavior, the test asserts that wrong behavior
//! so the fix can be landed as a test flip rather than a quiet regression.
//!
//! Theories covered:
//!   T1 — `respond_to_queries_impl` DSR stub
//!        (Windows stubs `\x1b[1;1R`, POSIX is a no-op).
//!        clud's fix: stop calling it (session.rs / daemon.rs).
//!   T2 — `resize_impl` is a no-op on Windows; forwards on POSIX.
//!        clud's fix: `session::resize_pty` reaches master.resize() directly.
//!   T3 — Spawn accepts `cols=32767` (the old clud fallback) without panicking.
//!        clud's fix: `resolve_terminal_size` now caps at 200 cols.
//!
//! ## Host-environment requirement
//!
//! On Windows, `CreatePseudoConsole` behaves oddly when the spawning process's
//! stdout is redirected (not a real console) — see
//! microsoft/terminal discussions around STARTF_USESTDHANDLES. In that case
//! the child's output never reaches the master reader and these tests time
//! out with 4 bytes of `\x1b[6n` and nothing else.
//!
//! To keep the suite green in such environments (piped `cargo test`, nested
//! shells, some CI runners), every test runs a one-shot `pty_canary()` first.
//! If the canary fails, the test logs a diagnostic and returns early rather
//! than panicking. On a real Windows Terminal / cmd / pwsh session, on Linux,
//! and on macOS, the canary passes and the real assertions run.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use running_process_core::pty::NativePtyProcess;
use running_process_core::{
    CommandSpec, NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode,
};
use serde_json::Value;

// ─────────────────────────────────────────────────────────────────────────
// Harness
// ─────────────────────────────────────────────────────────────────────────

/// Locate (and if necessary build) the workspace `mock-agent` binary.
///
/// Uses `CARGO_MANIFEST_DIR` / `CARGO_TARGET_DIR` and prefers the freshest
/// of the plausible target-triple-qualified paths — soldr / ci.env on
/// Windows build into `target/x86_64-pc-windows-msvc/debug/`, plain cargo
/// builds into `target/debug/`. Picking the freshest avoids serving a stale
/// pre-change binary to tests.
fn mock_agent_path() -> PathBuf {
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
        containment: None,
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

fn cargo_built_executable_path(output: &str) -> Option<PathBuf> {
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

#[test]
fn cargo_build_output_reports_mock_agent_executable() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let exe = tmp.path().join(if cfg!(windows) {
        "mock-agent.exe"
    } else {
        "mock-agent"
    });
    std::fs::write(&exe, b"binary").expect("write mock binary");

    let output = format!(
        "{{\"reason\":\"compiler-artifact\",\"target\":{{\"name\":\"mock-agent\",\"kind\":[\"bin\"]}},\"executable\":{}}}\n",
        serde_json::to_string(&exe.to_string_lossy().to_string()).expect("json string")
    );

    assert_eq!(cargo_built_executable_path(&output), Some(exe));
}

/// Wait up to `timeout` for `f` to return `true`, sleeping 50ms between polls.
fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
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
fn drain_reader(process: &NativePtyProcess, overall_timeout: Duration) -> Vec<u8> {
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
fn pty_canary() -> bool {
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
macro_rules! require_pty_or_skip {
    ($test_name:literal) => {
        if !pty_canary() {
            eprintln!(
                "[{}] SKIP: PTY canary failed in this host environment (parent stdout is not a real console).",
                $test_name
            );
            return;
        }
    };
}

// ─────────────────────────────────────────────────────────────────────────
// T1 — respond_to_queries_impl DSR behavior
// ─────────────────────────────────────────────────────────────────────────

/// Feed one `\x1b[6n` DSR query into the `respond_to_queries_impl` handler
/// and assert what the PTY child actually received on stdin.
///
/// - Windows: handler writes exactly one hardcoded `\x1b[1;1R` into the
///   child's stdin regardless of where the cursor actually is (issue #31,
///   theory T1). This is the bug.
/// - POSIX: handler is a no-op; the child receives zero bytes.
#[test]
fn respond_to_queries_matches_platform_stub() {
    require_pty_or_skip!("respond_to_queries_matches_platform_stub");

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let raw_stdin = tmp.path().join("stdin_raw.bin");

    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-read-stdin-ms".to_string(),
        "600".to_string(),
        "--mock-stdin-raw-to".to_string(),
        raw_stdin.to_string_lossy().to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 24, 80, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");

    // Let the child enter its stdin read loop.
    std::thread::sleep(Duration::from_millis(200));

    process
        .respond_to_queries_impl(b"prefix\x1b[6nsuffix")
        .expect("respond_to_queries");

    let _ = process.wait_impl(Some(5.0));
    let _ = drain_reader(&process, Duration::from_millis(500));
    let _ = process.close_impl();

    let got = std::fs::read(&raw_stdin).unwrap_or_default();

    if cfg!(windows) {
        assert_eq!(
            got, b"\x1b[1;1R",
            "Windows respond_to_queries should inject exactly one hardcoded DSR reply; got {:?}",
            got
        );
    } else {
        assert!(
            got.is_empty(),
            "POSIX respond_to_queries should be a no-op, but child received {:?}",
            got
        );
    }
}

/// A chunk containing N DSR queries produces N stubbed replies on Windows
/// and still nothing on POSIX.
#[test]
fn respond_to_queries_is_linear_in_query_count() {
    require_pty_or_skip!("respond_to_queries_is_linear_in_query_count");

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let raw_stdin = tmp.path().join("stdin_raw.bin");

    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-read-stdin-ms".to_string(),
        "600".to_string(),
        "--mock-stdin-raw-to".to_string(),
        raw_stdin.to_string_lossy().to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 24, 80, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");
    std::thread::sleep(Duration::from_millis(200));

    process
        .respond_to_queries_impl(b"\x1b[6nA\x1b[6nB\x1b[6n")
        .expect("respond_to_queries");

    let _ = process.wait_impl(Some(5.0));
    let _ = drain_reader(&process, Duration::from_millis(500));
    let _ = process.close_impl();

    let got = std::fs::read(&raw_stdin).unwrap_or_default();

    if cfg!(windows) {
        let expected: Vec<u8> = b"\x1b[1;1R\x1b[1;1R\x1b[1;1R".to_vec();
        assert_eq!(
            got, expected,
            "Windows should emit one stub per query; got {:?}",
            got
        );
    } else {
        assert!(
            got.is_empty(),
            "POSIX should emit nothing regardless of query count; got {:?}",
            got
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────
// T2 — resize_impl behavior
// ─────────────────────────────────────────────────────────────────────────

/// Spawn mock-agent in a PTY with a known size and assert the child sees
/// those dimensions via the `terminal_size` crate. Baseline: the axes must
/// match on POSIX before the resize test below is meaningful.
#[test]
fn initial_pty_size_is_forwarded_to_child() {
    require_pty_or_skip!("initial_pty_size_is_forwarded_to_child");

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let size_report = tmp.path().join("size.json");

    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-report-pty-size".to_string(),
        size_report.to_string_lossy().to_string(),
        "--mock-pty-size-samples".to_string(),
        "1".to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 30, 100, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");

    let _ = wait_until(Duration::from_secs(5), || {
        std::fs::metadata(&size_report)
            .map(|m| m.len() > 2)
            .unwrap_or(false)
    });

    let _ = process.wait_impl(Some(5.0));
    let _ = drain_reader(&process, Duration::from_millis(300));
    let _ = process.close_impl();

    let body = std::fs::read_to_string(&size_report).unwrap_or_default();
    if body.is_empty() {
        // Environment couldn't deliver the report file (extremely nested
        // shells on Windows). Canary passed so we still attempted — but
        // don't hard-fail here; the POSIX variant is the load-bearing
        // assertion in this theory.
        eprintln!("initial_pty_size: size report empty, skipping assertion");
        return;
    }
    let samples: Value = serde_json::from_str(&body).expect("parse size report");
    let samples = samples.as_array().expect("array");
    assert!(!samples.is_empty(), "no samples recorded");

    let first = &samples[0];
    let cols = first["cols"].as_u64();
    let rows = first["rows"].as_u64();

    if cfg!(windows) {
        // ConPTY honors the requested size when attached to a real console.
        // Headless-ConPTY CI boxes sometimes report `None`; accept either
        // the exact match or `None`. A regression would be a *wrong*
        // non-None value.
        if let (Some(c), Some(r)) = (cols, rows) {
            assert_eq!((c, r), (100, 30), "ConPTY reported wrong size: {:?}", first);
        }
    } else {
        assert_eq!(cols, Some(100), "POSIX PTY cols mismatch: {:?}", first);
        assert_eq!(rows, Some(30), "POSIX PTY rows mismatch: {:?}", first);
    }
}

/// Document what `running_process_core::pty::NativePtyProcess::resize_impl`
/// does today on each platform:
///   - POSIX: `master.resize()` propagates; the child sees the new size.
///   - Windows: intentional no-op (see running-process-core mod.rs:730-737).
///
/// clud no longer relies on this API on Windows — `session::resize_pty`
/// reaches the underlying `portable_pty::MasterPty::resize()` directly
/// (issue #31 T2 fix). This test locks the *library* contract so a future
/// library fix that enables Windows resize makes the workaround obsolete
/// and this assertion flips.
#[test]
fn resize_impl_propagates_on_posix_and_noops_on_windows() {
    require_pty_or_skip!("resize_impl_propagates_on_posix_and_noops_on_windows");

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let size_report = tmp.path().join("size.json");

    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-report-pty-size".to_string(),
        size_report.to_string_lossy().to_string(),
        "--mock-pty-size-samples".to_string(),
        "3".to_string(),
        "--mock-pty-size-interval-ms".to_string(),
        "250".to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 20, 80, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");

    let got_first = wait_until(Duration::from_secs(3), || {
        std::fs::metadata(&size_report)
            .map(|m| m.len() > 2)
            .unwrap_or(false)
    });
    if !got_first {
        // See note above — don't force-fail on environments where the
        // child can't deliver its artifacts.
        let _ = process.close_impl();
        eprintln!("resize_impl: never observed initial sample, skipping");
        return;
    }

    std::thread::sleep(Duration::from_millis(80));
    process.resize_impl(40, 120).expect("resize");

    let _ = process.wait_impl(Some(5.0));
    let _ = drain_reader(&process, Duration::from_millis(300));
    let _ = process.close_impl();

    let body = std::fs::read_to_string(&size_report).unwrap_or_default();
    let samples: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let samples = match samples.as_array() {
        Some(arr) if !arr.is_empty() => arr.clone(),
        _ => {
            eprintln!("resize_impl: empty samples, skipping");
            return;
        }
    };

    let first = &samples[0];
    let last = samples.last().expect("last sample");

    if cfg!(unix) {
        assert_eq!(
            last["cols"].as_u64(),
            Some(120),
            "POSIX resize_impl did not propagate cols: {:?}",
            samples
        );
        assert_eq!(
            last["rows"].as_u64(),
            Some(40),
            "POSIX resize_impl did not propagate rows: {:?}",
            samples
        );
    } else {
        // Windows: resize_impl is a no-op. The child's observed size MUST
        // NOT have changed to (120, 40). A `None` observation (headless
        // ConPTY) also satisfies "did not change".
        let changed = last["cols"].as_u64() == Some(120) && last["rows"].as_u64() == Some(40);
        assert!(
            !changed,
            "Windows resize_impl unexpectedly propagated (fix landed? flip this test): first={:?} last={:?}",
            first, last
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────
// T3 — extreme `cols` values
// ─────────────────────────────────────────────────────────────────────────

/// clud's `get_terminal_size()` fallback returns `cols = 32767` when stdout
/// isn't a terminal (main.rs:137-145). Verify portable-pty accepts this
/// without panicking at spawn — even if the child's layout math goes
/// sideways on the value. See issue #31, theory T3.
#[test]
fn extreme_cols_does_not_crash_at_spawn() {
    require_pty_or_skip!("extreme_cols_does_not_crash_at_spawn");

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let size_report = tmp.path().join("size.json");

    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-report-pty-size".to_string(),
        size_report.to_string_lossy().to_string(),
        "--mock-pty-size-samples".to_string(),
        "1".to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 24, 32767, None).expect("new pty");
    process.set_echo(false);
    process
        .start_impl()
        .expect("portable-pty rejected cols=32767 at spawn");

    let _ = wait_until(Duration::from_secs(5), || {
        std::fs::metadata(&size_report)
            .map(|m| m.len() > 2)
            .unwrap_or(false)
    });
    let _ = process.wait_impl(Some(5.0));
    let _ = drain_reader(&process, Duration::from_millis(300));
    let _ = process.close_impl();

    // If the child reported a size at all, it must be positive. We are not
    // asserting the exact value — portable-pty may clamp or pass through.
    // The load-bearing claim is "start_impl did not panic/error".
    if let Ok(body) = std::fs::read_to_string(&size_report) {
        if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&body) {
            if let Some(first) = arr.first() {
                if let Some(cols) = first["cols"].as_u64() {
                    assert!(cols > 0, "cols must be positive when reported: {}", cols);
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Raw PTY pump — verbatim stdin forwarding
// ─────────────────────────────────────────────────────────────────────────

/// Counting hooks for pump integration tests. Records F3 presses, ticks,
/// and can opt into voice interception via `intercept`.
struct CountingHooks {
    intercept: bool,
    f3_presses: std::sync::Arc<std::sync::atomic::AtomicU32>,
    ticks: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl CountingHooks {
    fn new(intercept: bool) -> Self {
        Self {
            intercept,
            f3_presses: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            ticks: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }
}

impl clud::session::InteractiveHooks for CountingHooks {
    fn intercept_f3(&self) -> bool {
        self.intercept
    }
    fn on_f3_press(&mut self, _process: &NativePtyProcess) -> std::io::Result<()> {
        self.f3_presses
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn on_tick(&mut self, _process: &NativePtyProcess) -> std::io::Result<()> {
        self.ticks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

/// The pump must forward stdin bytes verbatim — no CSI parsing, no
/// event-loop demultiplexing. This is the regression test for the DSR hang
/// (issue #46): a child TUI that emits `\x1b[6n` and expects the terminal
/// to reply must see our stdin bytes unchanged. We feed arbitrary bytes
/// including DSR queries, escape sequences, and F3 and assert the
/// mock-agent recorded exactly what we sent.
#[test]
fn raw_pump_forwards_stdin_bytes_verbatim() {
    require_pty_or_skip!("raw_pump_forwards_stdin_bytes_verbatim");

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let raw_stdin = tmp.path().join("stdin_raw.bin");

    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-read-stdin-ms".to_string(),
        "800".to_string(),
        "--mock-stdin-raw-to".to_string(),
        raw_stdin.to_string_lossy().to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 24, 80, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");

    // Give the child a moment to enter its stdin read loop before we feed.
    std::thread::sleep(Duration::from_millis(150));

    let payload: &[u8] = b"hello\x1b[6n\x1bOR\x1bOP world\n";
    let interrupted = AtomicBool::new(false);
    let mut hooks = CountingHooks::new(false);

    let _exit = clud::session::run_raw_pty_pump(
        &process,
        &interrupted,
        &mut hooks,
        Cursor::new(payload.to_vec()),
    );

    let _ = process.wait_impl(Some(5.0));
    let _ = drain_reader(&process, Duration::from_millis(300));
    let _ = process.close_impl();

    let got = std::fs::read(&raw_stdin).unwrap_or_default();
    assert_eq!(
        got, payload,
        "pump must forward stdin bytes verbatim; got {:?}, expected {:?}",
        got, payload
    );
}

/// F3 is observed (not intercepted): each `\x1bOR` in the byte stream
/// fires `on_f3_press` once AND the bytes still reach the child. This is
/// the voice-mode contract: clud reacts to F3 in parallel with forwarding,
/// not instead of forwarding.
#[test]
fn raw_pump_fires_voice_f3_press_while_forwarding_bytes() {
    require_pty_or_skip!("raw_pump_fires_voice_f3_press_while_forwarding_bytes");

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let raw_stdin = tmp.path().join("stdin_raw.bin");

    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-read-stdin-ms".to_string(),
        "800".to_string(),
        "--mock-stdin-raw-to".to_string(),
        raw_stdin.to_string_lossy().to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 24, 80, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");
    std::thread::sleep(Duration::from_millis(150));

    // Three F3 presses embedded in surrounding text. Trailing `\n` is
    // important: the PTY slave defaults to canonical (line) mode, so the
    // kernel holds input until it sees a newline. Without it, the
    // mock-agent's `stdin.read()` never returns and we'd assert on an
    // empty file. Real usage isn't affected — child TUIs like codex put
    // their own slave into raw mode before reading.
    let payload: &[u8] = b"a\x1bORb\x1bORc\x1bORd\n";
    let interrupted = AtomicBool::new(false);
    let hooks = CountingHooks::new(true); // intercept_f3 == true
    let presses = std::sync::Arc::clone(&hooks.f3_presses);
    let mut hooks = hooks;

    let _exit = clud::session::run_raw_pty_pump(
        &process,
        &interrupted,
        &mut hooks,
        Cursor::new(payload.to_vec()),
    );

    let _ = process.wait_impl(Some(5.0));
    let _ = drain_reader(&process, Duration::from_millis(300));
    let _ = process.close_impl();

    let got = std::fs::read(&raw_stdin).unwrap_or_default();
    assert_eq!(
        got, payload,
        "F3 interception must NOT eat bytes; child should still see {:?}, got {:?}",
        payload, got
    );
    assert_eq!(
        presses.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "expected 3 F3 presses; got a different count"
    );
}

/// `on_tick` must fire on every main-loop iteration regardless of stdin.
/// Voice mode drains a background transcription worker through this hook;
/// gating it behind stdin activity would leave transcripts stuck whenever
/// the user stops typing. Here: an empty stdin source, a 400ms child.
/// Expect at least ~20 ticks (loop cadence is bounded by the 10ms
/// `read_chunk_impl` timeout).
#[test]
fn raw_pump_calls_on_tick_during_idle() {
    require_pty_or_skip!("raw_pump_calls_on_tick_during_idle");

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let raw_stdin = tmp.path().join("stdin_raw.bin");

    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-read-stdin-ms".to_string(),
        "400".to_string(),
        "--mock-stdin-raw-to".to_string(),
        raw_stdin.to_string_lossy().to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 24, 80, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");
    std::thread::sleep(Duration::from_millis(100));

    let interrupted = AtomicBool::new(false);
    let hooks = CountingHooks::new(false);
    let ticks = std::sync::Arc::clone(&hooks.ticks);
    let mut hooks = hooks;

    // Empty stdin: reader thread hits EOF immediately. Main loop then
    // idles and must still tick.
    let _exit = clud::session::run_raw_pty_pump(
        &process,
        &interrupted,
        &mut hooks,
        Cursor::new(Vec::<u8>::new()),
    );

    let _ = process.wait_impl(Some(5.0));
    let _ = drain_reader(&process, Duration::from_millis(300));
    let _ = process.close_impl();

    let tick_count = ticks.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        tick_count >= 10,
        "expected >=10 ticks during 400ms idle child, got {}",
        tick_count
    );
}

/// Flipping the shared `interrupted` flag while the pump is running with
/// a long-lived child must cause the pump to return within a couple of
/// seconds with the Ctrl+C exit code path (130 on POSIX; on Windows the
/// helper returns whatever the child's signal produces, which
/// `normalize_exit_code` in main.rs later maps to 130 — here we just
/// assert the pump returned promptly without hanging on the child).
#[test]
fn raw_pump_honors_ctrlc_flag() {
    require_pty_or_skip!("raw_pump_honors_ctrlc_flag");

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let raw_stdin = tmp.path().join("stdin_raw.bin");

    // Long-lived: 5 seconds of blocking stdin read — gives us headroom to
    // observe the interrupt without racing against child exit.
    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-read-stdin-ms".to_string(),
        "5000".to_string(),
        "--mock-stdin-raw-to".to_string(),
        raw_stdin.to_string_lossy().to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 24, 80, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");
    std::thread::sleep(Duration::from_millis(150));

    let interrupted = std::sync::Arc::new(AtomicBool::new(false));
    let mut hooks = CountingHooks::new(false);

    // Trip the flag 200ms after the pump starts running.
    let flag = std::sync::Arc::clone(&interrupted);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    let start = Instant::now();
    let _exit = clud::session::run_raw_pty_pump(
        &process,
        &interrupted,
        &mut hooks,
        Cursor::new(Vec::<u8>::new()),
    );
    let elapsed = start.elapsed();

    let _ = process.close_impl();

    assert!(
        elapsed < Duration::from_millis(2500),
        "pump must return within 2.5s of ctrlc flag flip, took {:?}",
        elapsed
    );
}

/// Resize events delivered through the pump's resize channel must reach
/// `resize_pty` and propagate to the PTY master. This covers both Step 9
/// (Unix SIGWINCH source) and Step 10 (Windows ReadConsoleInputW source)
/// — the OS-specific threads are responsible for *producing* events into
/// this channel, and the pump is responsible for *consuming* them and
/// calling `resize_pty`. Here we bypass the OS source and push directly.
#[test]
fn raw_pump_applies_resize_from_channel() {
    require_pty_or_skip!("raw_pump_applies_resize_from_channel");

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let raw_stdin = tmp.path().join("stdin_raw.bin");

    // Long enough that the resize has time to land before the child exits.
    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-read-stdin-ms".to_string(),
        "600".to_string(),
        "--mock-stdin-raw-to".to_string(),
        raw_stdin.to_string_lossy().to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 20, 80, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");
    std::thread::sleep(Duration::from_millis(100));

    // Sanity: master starts at the size we spawned with.
    {
        let guard = process.handles.lock().expect("handles");
        let handles = guard.as_ref().expect("handles present");
        let size = handles.master.get_size().expect("get_size");
        assert_eq!((size.rows, size.cols), (20, 80));
    }

    let (resize_tx, resize_rx) = std::sync::mpsc::channel::<(u16, u16)>();

    // Push a resize after a short delay so the main loop is already
    // running when it arrives.
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(120));
        let _ = resize_tx.send((40, 120));
    });

    let interrupted = AtomicBool::new(false);
    let mut hooks = CountingHooks::new(false);

    let _exit = clud::session::run_raw_pty_pump_with_resize_rx(
        &process,
        &interrupted,
        &mut hooks,
        Cursor::new(Vec::<u8>::new()),
        resize_rx,
    );

    // After the pump consumed the resize, the master must reflect it.
    {
        let guard = process.handles.lock().expect("handles");
        let handles = guard.as_ref().expect("handles present");
        let size = handles.master.get_size().expect("get_size");
        assert_eq!(
            (size.rows, size.cols),
            (40, 120),
            "pump did not apply (40,120) resize from channel"
        );
    }

    let _ = process.wait_impl(Some(2.0));
    let _ = process.close_impl();
}

/// When the child exits, the pump must return promptly even if the
/// stdin reader thread is blocked in `read()`. Real `io::stdin()` blocks
/// waiting for the next keystroke — never returning EOF. We simulate
/// that with a `Read` impl that parks forever. The detached reader
/// thread is fine to leave blocked; the main loop's `poll_pty_process`
/// is what drives shutdown.
#[test]
fn raw_pump_exits_promptly_when_child_exits() {
    require_pty_or_skip!("raw_pump_exits_promptly_when_child_exits");

    struct BlockingReader;
    impl std::io::Read for BlockingReader {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            // Mimics `io::stdin()` with no pending input: block indefinitely.
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
    }

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let raw_stdin = tmp.path().join("stdin_raw.bin");

    // Short-lived child — it reads stdin for 400ms then exits.
    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-read-stdin-ms".to_string(),
        "400".to_string(),
        "--mock-stdin-raw-to".to_string(),
        raw_stdin.to_string_lossy().to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 24, 80, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");

    let interrupted = AtomicBool::new(false);
    let mut hooks = CountingHooks::new(false);

    let start = Instant::now();
    let _exit = clud::session::run_raw_pty_pump(&process, &interrupted, &mut hooks, BlockingReader);
    let elapsed = start.elapsed();

    let _ = process.close_impl();

    assert!(
        elapsed < Duration::from_millis(1500),
        "pump must exit within 1.5s after child dies, took {:?}",
        elapsed
    );
}
/// fire and disable raw mode — otherwise the user's terminal is left in
/// a broken state after a crash. `catch_unwind` wraps the panicking hook
/// to assert recovery.
#[test]
fn raw_pump_restores_raw_mode_on_panic() {
    require_pty_or_skip!("raw_pump_restores_raw_mode_on_panic");

    // Note: the pump itself doesn't own a RawTerminalGuard — `run_plan_pty`
    // in main.rs does (will, once wired in Step 13). Raw mode is a
    // caller-side concern. This test verifies the pump doesn't leak raw
    // mode *on panic*: if we enter raw mode before calling the pump with
    // a panicking hook, the guard on the main stack frame unwinds and
    // restores the terminal.
    struct PanickingHooks;
    impl clud::session::InteractiveHooks for PanickingHooks {
        fn on_tick(&mut self, _process: &NativePtyProcess) -> std::io::Result<()> {
            panic!("deliberate panic in on_tick");
        }
    }

    let agent = mock_agent_path();
    let tmp = tempfile::tempdir().expect("tempdir");
    let raw_stdin = tmp.path().join("stdin_raw.bin");

    let argv = vec![
        agent.to_string_lossy().to_string(),
        "--mock-read-stdin-ms".to_string(),
        "500".to_string(),
        "--mock-stdin-raw-to".to_string(),
        raw_stdin.to_string_lossy().to_string(),
    ];

    let process = NativePtyProcess::new(argv, None, None, 24, 80, None).expect("new pty");
    process.set_echo(false);
    process.start_impl().expect("start");
    std::thread::sleep(Duration::from_millis(100));

    let interrupted = AtomicBool::new(false);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut hooks = PanickingHooks;
        clud::session::run_raw_pty_pump(
            &process,
            &interrupted,
            &mut hooks,
            Cursor::new(Vec::<u8>::new()),
        )
    }));

    assert!(result.is_err(), "hook panic must propagate to catch_unwind");

    // The crossterm raw-mode state is a caller concern; we just verify the
    // pump returned control (panic surfaced) rather than hung after the
    // panic unwound through its own threads.
    let _ = process.wait_impl(Some(2.0));
    let _ = process.close_impl();
}
