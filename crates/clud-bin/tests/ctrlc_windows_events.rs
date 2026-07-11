//! Issue #517: end-to-end verification that `CTRL_BREAK_EVENT` is
//! reported with the correct `CtrlEventKind` reason through the real
//! production `startup::install_windows_ctrl_event_probe` path.
//!
//! Uses raw `windows::Win32::System::Console::GenerateConsoleCtrlEvent`
//! (not `running_process::NativeProcess`) so the console-control event
//! targets the exact probe process group directly, matching the actual
//! Win32 API under test — exempted in `ci/banned_imports.py`.
//!
//! `CTRL_C_EVENT` is intentionally NOT exercised here: per MSDN it only
//! reliably targets process group 0 (every process attached to the
//! console, including the test harness itself), which is flaky on
//! hosted CI runners without a real interactive console. `CTRL_BREAK_EVENT`
//! targets a specific process group id and is fully unattended-safe.
#![cfg(windows)]

use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

use windows::Win32::System::Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT};

/// Spawning with a new process group is required for
/// `GenerateConsoleCtrlEvent` to target only this child (its pid becomes
/// its process group id) rather than every process on the console.
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

#[test]
fn ctrl_break_event_is_reported_as_ctrl_break() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_clud-ctrlc-probe"))
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
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

    let pid = child.id();
    // SAFETY: `GenerateConsoleCtrlEvent` with a valid process group id is
    // a plain Win32 API call; no memory safety concerns.
    unsafe {
        GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid).expect("GenerateConsoleCtrlEvent failed");
    }

    let mut kind_line = String::new();
    reader
        .read_line(&mut kind_line)
        .expect("read reported kind from probe");
    let status = child.wait().expect("wait for probe");
    assert!(status.success(), "probe must exit 0, got {status:?}");

    assert_eq!(
        kind_line.trim(),
        "CtrlBreak",
        "CTRL_BREAK_EVENT must be reported as CtrlBreak"
    );
}
