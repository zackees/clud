//! Windows half of `crate::shim_main`'s relay. Windows has no `exec`, so
//! the shim runs the real binary as a child and returns its exit code.
//!
//! This file keeps the `shim_main.rs` basename on purpose:
//! `ci/banned_imports.py` exempts the relay's raw `std::process` use by
//! basename (#406 / #409).

use std::path::Path;
use std::process::{Child, Command};

/// Run `command` and return its exit code. Only a spawn failure returns 126.
pub fn exec(label: &str, path: &Path, mut command: Command) -> i32 {
    match command.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!(
                "clud {label} shim: failed to exec {}: {error}",
                path.display()
            );
            126
        }
    }
}

/// Wait for `child`, record its exit code, and return it.
pub fn wait_child(
    label: &str,
    path: &Path,
    mut child: Child,
    recorder: &crate::shim_telemetry::Recorder,
) -> i32 {
    let code = match child.wait() {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!(
                "clud {label} shim: failed to wait for {}: {error}",
                path.display()
            );
            1
        }
    };
    recorder.record(code);
    code
}
