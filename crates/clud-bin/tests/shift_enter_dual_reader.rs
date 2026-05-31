//! RED test for the dual-reader race on Windows that prevents
//! Shift+Enter from reliably producing `\n` even after issue #141
//! landed the `ReadConsoleInputW` translator.
//!
//! The production wiring in `runner::run_plan_pty` spawns
//! `console_input::spawn_console_input_reader()` (which calls
//! `ReadConsoleInputW` and translates Shift+Enter to `\n`) *and* the
//! pump's stdin reader thread in `session::run_raw_pty_pump_full_verbose`
//! (which calls `io::stdin().read(...)`, i.e. `ReadFile` on the same
//! STDIN handle). Both consume the same console input queue. The
//! `ReadFile`-based reader sees Shift+Enter as a bare `\r` byte because
//! conhost strips modifier state before producing the byte stream — so
//! whichever reader wins the race for a given keystroke dictates what
//! the child PTY ultimately receives.
//!
//! This test makes the race explicit:
//!
//! 1. Spawn the production `console_input` reader (the same one
//!    `runner.rs:534` uses).
//! 2. Spawn an `io::stdin()` reader thread (the same one
//!    `session.rs:581` uses).
//! 3. Inject N synthetic Shift+Enter `KEY_EVENT_RECORD`s into the
//!    test process's STDIN console queue via `WriteConsoleInputW`.
//! 4. Drain both channels for a fixed window.
//! 5. Assert that every injected Shift+Enter surfaces as `\n` on the
//!    aggregated byte stream.
//!
//! With the bug present, the `io::stdin()` reader will steal some
//! (often most) of the events and produce `\r` for them, so the
//! `\n` count drops below N. The test asserts the strict invariant:
//! all N must survive as `\n`.

#![cfg(windows)]

use std::io::{IsTerminal, Read};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clud::console_input::spawn_console_input_reader;
use windows::Win32::System::Console::{
    GetStdHandle, WriteConsoleInputW, INPUT_RECORD, INPUT_RECORD_0, KEY_EVENT, KEY_EVENT_RECORD,
    KEY_EVENT_RECORD_0, STD_INPUT_HANDLE,
};
use windows_core::BOOL;

const VK_RETURN: u16 = 0x0D;
const SHIFT_PRESSED: u32 = 0x0010;

fn shift_enter_record(key_down: bool) -> INPUT_RECORD {
    INPUT_RECORD {
        EventType: KEY_EVENT as u16,
        Event: INPUT_RECORD_0 {
            KeyEvent: KEY_EVENT_RECORD {
                bKeyDown: BOOL(if key_down { 1 } else { 0 }),
                wRepeatCount: 1,
                wVirtualKeyCode: VK_RETURN,
                wVirtualScanCode: 0,
                uChar: KEY_EVENT_RECORD_0 {
                    UnicodeChar: b'\r' as u16,
                },
                dwControlKeyState: SHIFT_PRESSED,
            },
        },
    }
}

/// RED: `clud` is supposed to translate Shift+Enter into `\n` for the
/// child PTY (issue #141). When the runner wires both the
/// `console_input` `ReadConsoleInputW` worker *and* the pump's
/// `io::stdin()` `ReadFile` thread against the same STDIN handle, the
/// `ReadFile` path sees Shift+Enter as a bare `\r` (conhost strips
/// modifiers before the byte stream is produced). Whichever reader
/// wins the race per keystroke decides whether the child sees `\n`
/// (the modifier-aware translator won) or `\r` (the byte-stream reader
/// won). The user-visible symptom: "Shift+Enter still doesn't work on
/// Windows" even though the unit tests for `translate()` are green.
///
/// This test pins the desired invariant: with both readers active on a
/// real console, every injected Shift+Enter must surface as `\n` on
/// the combined output. Today it fails because the byte-stream reader
/// steals a fraction of the events and emits `\r`.
#[test]
fn shift_enter_survives_dual_reader_race() {
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "shift_enter_survives_dual_reader_race: SKIP \
             (stdin not a real console in this test runner)"
        );
        return;
    }

    let (mut console_handle, _mode_guard) =
        spawn_console_input_reader().expect("spawn_console_input_reader");
    let console_rx = console_handle
        .take_receiver()
        .expect("ConsoleInputHandle::take_receiver");

    // Mimic the pump's stdin reader thread in
    // `session::run_raw_pty_pump_full_verbose` (session.rs:581) — same
    // call shape, same target handle, no normalization.
    let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>();
    let _stdin_thread = std::thread::spawn(move || {
        let mut reader = std::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if stdin_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Let both readers settle on the input queue.
    std::thread::sleep(Duration::from_millis(50));

    const N: usize = 16;
    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) }.expect("GetStdHandle");
    for _ in 0..N {
        // Mirror what a real Shift+Enter keystroke produces: a key-down
        // followed by a key-up record.
        let records = [shift_enter_record(true), shift_enter_record(false)];
        let mut written: u32 = 0;
        unsafe { WriteConsoleInputW(handle, &records, &mut written) }.expect("WriteConsoleInputW");
        assert_eq!(written, 2, "WriteConsoleInputW must write both records");
        // Small delay so each Shift+Enter event is independently
        // observable rather than getting batched into one ReadFile.
        std::thread::sleep(Duration::from_millis(8));
    }

    // Drain both channels for a fixed window.
    let mut combined: Vec<u8> = Vec::new();
    let deadline = Instant::now() + Duration::from_millis(800);
    while Instant::now() < deadline {
        let mut progress = false;
        while let Ok(chunk) = console_rx.try_recv() {
            combined.extend_from_slice(&chunk);
            progress = true;
        }
        while let Ok(chunk) = stdin_rx.try_recv() {
            combined.extend_from_slice(&chunk);
            progress = true;
        }
        if !progress {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    let nl_count = combined.iter().filter(|&&b| b == b'\n').count();
    let cr_count = combined.iter().filter(|&&b| b == b'\r').count();

    assert_eq!(
        nl_count, N,
        "RED: only {nl_count}/{N} Shift+Enter events survived as \\n; \
         {cr_count} surfaced as \\r (stolen by the io::stdin() ReadFile \
         reader). Combined bytes: {:?}",
        combined
    );
}
