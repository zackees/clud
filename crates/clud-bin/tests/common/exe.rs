//! Runtime resolution of workspace binaries for integration tests.
//!
//! Included standalone via `#[path = "../common/exe.rs"] mod exe;` in a
//! category `main.rs` rather than through `common/mod.rs`, so a test target
//! that needs only this helper does not drag in (and warn about) the PTY
//! helpers next door. Members reach it as `use crate::exe;`.
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

/// Resolve `name` against `CLUD_TEST_BIN_DIR`, then the optional compile-time
/// `CARGO_BIN_EXE_*` value, then the sibling target-directory layout.
///
/// Callers pass `option_env!` themselves because `CARGO_BIN_EXE_*` is only
/// expanded inside the integration-test crate that built the binary. Static
/// checks do not always define it, while actual test builds normally do.
pub fn bin_path(name: &str, compiled: Option<&str>) -> PathBuf {
    bundled(name)
        .or_else(|| compiled.map(PathBuf::from))
        .unwrap_or_else(|| sibling_bin_path(name))
}

/// Resolve a workspace binary that this test crate has **no**
/// `CARGO_BIN_EXE_*` for, because it belongs to another package —
/// `testbins/daemon-stub` and friends.
///
/// Same precedence as [`bin_path`]: the bundle's bin dir first, then the local
/// `target/<triple>/debug/` layout beside the harness. Getting only the second
/// half of that wrong is not a compile error, it is a *runtime* one that shows
/// up on the exec runners and nowhere else — the bundle puts harnesses in
/// `bundle/tests/` and workspace binaries in a sibling dir, so
/// "one level up from `deps/`" resolves to nothing there.
pub fn sibling_bin_path(name: &str) -> PathBuf {
    if let Some(path) = bundled(name) {
        return path;
    }
    let mut dir = std::env::current_exe().expect("current test exe");
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    dir.join(exe_file_name(name))
}

fn bundled(name: &str) -> Option<PathBuf> {
    let dir = std::env::var_os("CLUD_TEST_BIN_DIR")?;
    let candidate = PathBuf::from(dir).join(exe_file_name(name));
    candidate.is_file().then_some(candidate)
}

fn exe_file_name(name: &str) -> String {
    let ext = if cfg!(windows) { ".exe" } else { "" };
    format!("{name}{ext}")
}
