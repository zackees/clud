//! Direct terminal output helpers used by the PTY runner.

use std::io::{self, Write};

pub(super) fn write_terminal_bytes(bytes: &[u8]) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(bytes);
    let _ = out.flush();
}
