//! Integration test for `clud symbols`.
//!
//! Drops two canned crash-report JSON files into a tempdir, points
//! `CLUD_DAEMON_STATE_DIR` at it, then exercises the subcommands as a
//! subprocess of the installed `clud` binary so the full clap routing
//! is covered.

use std::fs;
use std::time::Duration;

use running_process::{
    CommandSpec, NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode,
};
use tempfile::TempDir;

fn write_report(dir: &std::path::Path, name: &str, backtrace: &str) {
    let path = dir.join(name);
    let body = serde_json::json!({
        "version": "0.0.0",
        "role": "test",
        "kind": "panic",
        "pid": 12345,
        "cwd": null,
        "args": [],
        "timestamp_unix_ms": 100u64,
        "panic_message": "synthetic",
        "backtrace": backtrace,
    });
    fs::write(&path, body.to_string()).expect("write report");
}

/// Spawn `clud <args>` via running_process::NativeProcess, drain its
/// merged stdout+stderr into a `String`, and return `(exit_code, output)`.
fn run_clud(args: &[&str], state_dir: &std::path::Path) -> (i32, String) {
    let mut argv = vec![env!("CARGO_BIN_EXE_clud").to_string()];
    argv.extend(args.iter().map(|s| s.to_string()));

    let mut env: Vec<(String, String)> = std::env::vars().collect();
    env.retain(|(k, _)| k != "CLUD_DAEMON_STATE_DIR" && k != "CLUD_NO_DAEMON");
    env.push((
        "CLUD_DAEMON_STATE_DIR".to_string(),
        state_dir.to_string_lossy().into_owned(),
    ));
    env.push(("CLUD_NO_DAEMON".to_string(), "1".to_string()));

    let process = NativeProcess::new(ProcessConfig {
        command: CommandSpec::Argv(argv),
        cwd: None,
        env: Some(env),
        capture: true,
        stderr_mode: StderrMode::Stdout,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
    });
    process.start().expect("spawn clud");

    let mut buf = Vec::<u8>::new();
    loop {
        match process.read_combined(Some(Duration::from_millis(100))) {
            ReadStatus::Line(event) => {
                buf.extend_from_slice(&event.line);
                buf.push(b'\n');
            }
            ReadStatus::Timeout => {
                if process.returncode().is_some() {
                    break;
                }
            }
            ReadStatus::Eof => break,
        }
    }
    let exit = process
        .wait(Some(Duration::from_secs(15)))
        .expect("wait clud");
    (exit, String::from_utf8_lossy(&buf).into_owned())
}

#[test]
fn symbols_verify_all_passes_when_every_report_has_file_line_frames() {
    let tmp = TempDir::new().unwrap();
    let state_dir = tmp.path().join("state");
    let crashes_dir = state_dir.join("crashes");
    fs::create_dir_all(&crashes_dir).unwrap();
    write_report(
        &crashes_dir,
        "100-test-1.json",
        "   0: clud::main::h1234\n             at /home/u/clud/src/main.rs:42:5\n",
    );
    write_report(
        &crashes_dir,
        "200-test-2.json",
        "   0: clud::main::h5678\n             at /home/u/clud/src/lib.rs:99:1\n",
    );

    let (exit, output) = run_clud(&["symbols", "verify", "--all"], &state_dir);
    assert_eq!(exit, 0, "expected exit 0; output: {output}");
    assert!(
        output.contains("clud symbols: OK"),
        "expected OK footer; got: {output}"
    );
}

#[test]
fn symbols_verify_all_fails_when_any_report_is_unsymbolicated() {
    let tmp = TempDir::new().unwrap();
    let state_dir = tmp.path().join("state");
    let crashes_dir = state_dir.join("crashes");
    fs::create_dir_all(&crashes_dir).unwrap();
    write_report(
        &crashes_dir,
        "100-test-1.json",
        "   0: clud::main::h1234\n             at /home/u/clud/src/main.rs:42:5\n",
    );
    write_report(
        &crashes_dir,
        "200-test-2.json",
        "   0: 0x7fffabcd1234\n   1: 0x7fffabcd5678\n",
    );

    let (exit, output) = run_clud(&["symbols", "verify", "--all"], &state_dir);
    assert_ne!(exit, 0, "expected non-zero exit; output: {output}");
    assert!(
        output.contains("clud symbols: FAIL"),
        "expected FAIL footer; got: {output}"
    );
}

#[test]
fn symbols_install_inspects_only_most_recent_report() {
    let tmp = TempDir::new().unwrap();
    let state_dir = tmp.path().join("state");
    let crashes_dir = state_dir.join("crashes");
    fs::create_dir_all(&crashes_dir).unwrap();
    // Older report is unsymbolicated; newest is symbolicated. `install`
    // should only look at the newest and return OK.
    write_report(
        &crashes_dir,
        "100-test-old.json",
        "   0: 0x7fffabcd1234\n   1: 0x7fffabcd5678\n",
    );
    write_report(
        &crashes_dir,
        "999-test-new.json",
        "   0: clud::main::h1234\n             at /home/u/clud/src/main.rs:42:5\n",
    );

    let (exit, output) = run_clud(&["symbols", "install"], &state_dir);
    assert_eq!(exit, 0, "expected exit 0; output: {output}");
    assert!(
        output.contains("999-test-new.json"),
        "should mention newest report; got: {output}"
    );
    assert!(
        !output.contains("100-test-old.json"),
        "should NOT mention older report; got: {output}"
    );
}

#[test]
fn symbols_bare_prints_summary() {
    let tmp = TempDir::new().unwrap();
    let state_dir = tmp.path().join("state");
    let crashes_dir = state_dir.join("crashes");
    fs::create_dir_all(&crashes_dir).unwrap();
    write_report(
        &crashes_dir,
        "100-test-1.json",
        "   0: clud::main::h1234\n             at /home/u/clud/src/main.rs:42:5\n",
    );
    write_report(
        &crashes_dir,
        "200-test-2.json",
        "   0: 0x7fffabcd1234\n   1: 0x7fffabcd5678\n",
    );

    let (exit, output) = run_clud(&["symbols"], &state_dir);
    assert_eq!(exit, 0, "bare summary should exit 0");
    assert!(output.contains("total reports: 2"), "got: {output}");
    assert!(
        output.contains("reports with file:line frames: 1"),
        "got: {output}"
    );
    assert!(
        output.contains("reports without file:line frames: 1"),
        "got: {output}"
    );
}
