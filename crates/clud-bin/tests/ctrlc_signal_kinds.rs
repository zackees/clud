//! Issue #517: end-to-end verification that SIGINT/SIGTERM/SIGHUP/SIGQUIT
//! are captured with the correct `CtrlEventKind` reason through the real
//! production `startup::install_ctrl_c_flag` path.
//!
//! Uses raw `libc::kill` (not `running_process::NativeProcess`) so the
//! signal targets the exact probe pid directly. `NativeProcess` always
//! spawns under containment (a Job Object on Windows; irrelevant here
//! since this file is Unix-only), which is the wrong shape for a test
//! whose entire point is "does *this* pid receive *this* signal and
//! record the right reason" — exempted in `ci/banned_imports.py`.
#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

fn probe_reports(signal: libc::c_int, expected_kind: &str) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_clud-ctrlc-probe"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn clud-ctrlc-probe");

    let mut reader = BufReader::new(child.stdout.take().expect("probe stdout"));
    let mut ready_line = String::new();
    reader
        .read_line(&mut ready_line)
        .expect("read ready line from probe");
    assert_eq!(
        ready_line.trim(),
        "ready",
        "probe must print 'ready' once the handler is installed"
    );

    let pid = child.id() as libc::pid_t;
    // SAFETY: `kill` with a valid pid and signal number is a plain
    // syscall wrapper; no memory safety concerns.
    let rc = unsafe { libc::kill(pid, signal) };
    assert_eq!(rc, 0, "kill(pid={pid}, sig={signal}) failed");

    let mut kind_line = String::new();
    reader
        .read_line(&mut kind_line)
        .expect("read reported kind from probe");
    let status = child.wait().expect("wait for probe");
    assert!(status.success(), "probe must exit 0, got {status:?}");

    assert_eq!(
        kind_line.trim(),
        expected_kind,
        "signal {signal} must be reported as {expected_kind}"
    );
}

#[test]
fn sigint_is_reported_as_ctrl_c() {
    probe_reports(libc::SIGINT, "CtrlC");
}

#[test]
fn sigterm_is_reported_as_term() {
    probe_reports(libc::SIGTERM, "Term");
}

#[test]
fn sighup_is_reported_as_hup() {
    probe_reports(libc::SIGHUP, "Hup");
}

#[test]
fn sigquit_is_reported_as_quit() {
    // Also verifies SIGQUIT/SIGHUP no longer kill the process under the
    // OS default disposition (core dump / terminate): `probe_reports`
    // asserts `status.success()`, which a signal-killed process fails.
    probe_reports(libc::SIGQUIT, "Quit");
}
