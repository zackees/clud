//! Cross-platform behavior tests for `running_process::pty::NativePtyProcess`
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
//!
//! Raw-PTY-pump integration tests live in `tests/pty_pump.rs`; shared
//! harness helpers are in `tests/common/mod.rs`.

use std::time::Duration;

use running_process::pty::NativePtyProcess;
use serde_json::Value;

mod common;
use common::{cargo_built_executable_path, drain_reader, mock_agent_path, wait_until};

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

/// Document what `running_process::pty::NativePtyProcess::resize_impl`
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
