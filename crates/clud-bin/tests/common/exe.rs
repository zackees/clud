//! Runtime resolution of workspace binaries for integration tests.
//!
//! Included standalone via `#[path = "common/exe.rs"] mod exe;` rather than
//! through `common/mod.rs`, so a test crate that needs only this helper does
//! not drag in (and warn about) the PTY helpers next door.
//!
//! ## Why this exists
//!
//! `env!("CARGO_BIN_EXE_<name>")` bakes the **builder's** absolute path into the
//! test binary at compile time, and Cargo offers no runtime override. That is
//! fine when the machine that compiles the harness is the machine that runs it.
//!
//! CI no longer works that way: test harnesses are compiled once on a Linux
//! runner and executed on native macOS/Windows runners, where
//! `/home/runner/work/.../target/debug/clud` does not exist (see
//! `docs/architecture/ci.md`). `CLUD_TEST_BIN_DIR` is the runtime override the
//! exec runner sets -- the compile-time constant stays as the fallback, so a
//! plain local `cargo test` behaves exactly as before.

#![allow(dead_code)]

use std::path::PathBuf;

/// Resolve `name` against `CLUD_TEST_BIN_DIR`, falling back to the
/// compile-time `CARGO_BIN_EXE_*` value passed as `compiled`.
///
/// Callers pass the `env!` themselves because `CARGO_BIN_EXE_*` is only
/// expanded inside the integration-test crate that Cargo built the binary for.
pub fn bin_path(name: &str, compiled: &str) -> PathBuf {
    if let Some(dir) = std::env::var_os("CLUD_TEST_BIN_DIR") {
        let ext = if cfg!(windows) { ".exe" } else { "" };
        let candidate = PathBuf::from(dir).join(format!("{name}{ext}"));
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(compiled)
}
