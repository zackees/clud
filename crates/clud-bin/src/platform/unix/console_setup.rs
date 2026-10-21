//! Unix half of `crate::console_setup`: POSIX has no console modes, so
//! every stream reads as "not a console" and the guard restores nothing.

use crate::console_setup::{OutputConsoleModes, OutputStream};

/// The process's real standard output streams.
pub struct StdConsole;

impl OutputConsoleModes for StdConsole {
    fn get(&self, _stream: OutputStream) -> Option<u32> {
        None
    }

    fn set(&mut self, _stream: OutputStream, _mode: u32) {}
}

/// RAII guard that restores the original console input and output modes on
/// drop. Holds nothing on POSIX.
pub struct ConsoleVtGuard {}

/// Empty, but present: shared code ends the guard's scope with an explicit
/// `drop`, which must mean the same on every host (Windows restores modes).
impl Drop for ConsoleVtGuard {
    fn drop(&mut self) {}
}

/// No-op on POSIX.
pub fn enable_console_vt_input() -> ConsoleVtGuard {
    ConsoleVtGuard {}
}
