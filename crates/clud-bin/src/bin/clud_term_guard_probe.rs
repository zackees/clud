//! Test-only helper for #1705's terminal-restore guard.
//!
//! Runs the real production path: [`clud::multicall::maybe_run`] first, so
//! when the session spawns `current_exe() __term-guard ...` this probe runs
//! the guard exactly as `clud` would; then
//! [`clud::session::RawTerminalGuard::enter`], which snapshots the terminal,
//! enters raw mode and starts the guard. It then turns on the input modes a
//! child TUI enables, as the child's output would reach the terminal.
//!
//! `tests/signals/term_guard_restore.rs` runs it on a pseudo-terminal (Unix)
//! or its own console (Windows), then force-kills it or lets it exit.
//!
//! Usage: `clud-term-guard-probe hold|clean`, and on Windows also
//! `clud-term-guard-probe keep <pid>`. Protocol lines go to stderr:
//! `ORIGINAL <hex>` and `RAW <hex>` (the terminal settings), then
//! `ARMED <guard pid>` or `ARMED none`. `hold` then waits up to 60 s to be
//! killed; `clean` restores the terminal and exits 0, printing `CLEAN`.
//! `keep <pid>` attaches to that process's console and prints `MODE <hex>`
//! for its input mode every 50 ms for up to 30 s, on stdout.

use std::ffi::OsString;
use std::io::Write;
use std::time::Duration;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn main() {
    let argv: Vec<OsString> = std::env::args_os().collect();
    if let Some(code) = clud::multicall::maybe_run(&argv) {
        std::process::exit(code);
    }
    let mode = argv
        .get(1)
        .and_then(|arg| arg.to_str())
        .unwrap_or("hold")
        .to_string();
    #[cfg(windows)]
    {
        if mode == "keep" {
            let pid = argv
                .get(2)
                .and_then(|arg| arg.to_str())
                .and_then(|arg| arg.parse().ok())
                .expect("keep <pid>");
            console::keep(pid);
            return;
        }
        console::adopt_own_console();
    }

    let original = clud::term_guard::initial_modes().expect("a terminal on stdin");
    let guard = clud::session::RawTerminalGuard::enter().expect("enter raw mode");
    let raw = clud::term_guard::current_modes().expect("raw terminal settings");
    // What a child TUI turns on: any-motion SGR mouse, focus, bracketed paste.
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(b"\x1b[?1003h\x1b[?1006h\x1b[?1004h\x1b[?2004h");
    let _ = stdout.flush();
    let armed = guard.wait_restore_guard(Duration::from_secs(20));
    eprintln!("ORIGINAL {}", hex(&original));
    eprintln!("RAW {}", hex(&raw));
    match armed {
        Some(pid) => eprintln!("ARMED {pid}"),
        None => eprintln!("ARMED none"),
    }
    if mode == "clean" {
        drop(guard);
        eprintln!("CLEAN");
        return;
    }
    std::thread::sleep(Duration::from_secs(60));
}

#[cfg(windows)]
mod console {
    use std::fs::{File, OpenOptions};
    use std::io::Write;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::time::{Duration, Instant};

    use winapi::um::consoleapi::GetConsoleMode;
    use winapi::um::processenv::{GetStdHandle, SetStdHandle};
    use winapi::um::winbase::{STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
    use winapi::um::wincon::{AttachConsole, FreeConsole};
    use winapi::um::winnt::HANDLE;

    fn open(name: &str) -> File {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(name)
            .unwrap_or_else(|e| panic!("open {name}: {e}"))
    }

    /// The test starts the probe in a new console with piped stdio; make
    /// that console its stdin and stdout, as a terminal session's are.
    pub fn adopt_own_console() {
        let input = open("CONIN$");
        let output = open("CONOUT$");
        // SAFETY: the handles are leaked on purpose: they stay the process's
        // standard handles for its whole life.
        unsafe {
            SetStdHandle(STD_INPUT_HANDLE, input.as_raw_handle() as HANDLE);
            SetStdHandle(STD_OUTPUT_HANDLE, output.as_raw_handle() as HANDLE);
        }
        std::mem::forget(input);
        std::mem::forget(output);
    }

    /// Attach to `pid`'s console and report its input mode on the stdout
    /// handle the test gave this process.
    pub fn keep(pid: u32) {
        // SAFETY: the stdout handle the test passed, taken before attaching
        // so a console attach cannot replace it.
        let mut report = unsafe { File::from_raw_handle(GetStdHandle(STD_OUTPUT_HANDLE) as _) };
        unsafe {
            FreeConsole();
            if AttachConsole(pid) == 0 {
                let _ = writeln!(report, "ATTACH-FAILED");
                return;
            }
        }
        let input = open("CONIN$");
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            let mut mode = 0u32;
            // SAFETY: GetConsoleMode only writes `mode`.
            if unsafe { GetConsoleMode(input.as_raw_handle() as HANDLE, &mut mode) } != 0 {
                let line = format!("MODE {}\n", super::hex(&mode.to_le_bytes()));
                if report.write_all(line.as_bytes()).is_err() {
                    return;
                }
                let _ = report.flush();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
