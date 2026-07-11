//! Test-only helper for issue #517's automatable interrupt-reason tests.
//!
//! Installs the real production interrupt path
//! (`clud::startup::install_ctrl_c_flag`, which also wires up the
//! Windows console-control probe and the Unix SIGTERM/SIGHUP/SIGQUIT
//! probe), then reports which `CtrlEventKind` fired. Integration tests
//! drive this binary with real OS signals / console-control events
//! (`libc::kill` on Unix, `GenerateConsoleCtrlEvent` on Windows) instead
//! of re-implementing clud's handler, so the tests exercise the actual
//! production code path.
//!
//! Protocol on stdout: prints `ready` once the handler is installed
//! (flushed immediately), then blocks until interrupted or a 30s
//! timeout, then prints the observed `CtrlEventKind` in `Debug` format
//! (e.g. `CtrlC`), or `none` if nothing was ever recorded, or `timeout`
//! if 30s elapsed with no interrupt. Always exits 0 — the test harness
//! reads the printed line, not the exit code.

use std::io::Write;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

fn main() {
    let interrupted = clud::startup::install_ctrl_c_flag(false);
    println!("ready");
    let _ = std::io::stdout().flush();

    let start = Instant::now();
    while !interrupted.load(Ordering::SeqCst) {
        if start.elapsed() > Duration::from_secs(30) {
            println!("timeout");
            let _ = std::io::stdout().flush();
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    match clud::ctrl_c_track::observed_event_kind() {
        Some(kind) => println!("{kind:?}"),
        None => println!("none"),
    }
    let _ = std::io::stdout().flush();
}
