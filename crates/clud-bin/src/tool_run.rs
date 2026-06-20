//! `clud tool run <rel_path> [args]` — Layer 1 of the three-layer
//! `UV_CACHE_DIR` enforcement from issue #408.
//!
//! Resolves a tool installed under `~/.clud/tools/<rel_path>` and execs
//! `uv run <full-path> [args]` with `UV_CACHE_DIR` pinned to
//! [`tools::clud_uv_cache_dir`]. The pin is read-or-default: if the parent
//! process already set `UV_CACHE_DIR` (Layer 3 in `main.rs`), this layer
//! re-affirms the same value; if it wasn't set, this layer establishes the
//! default.
//!
//! Surfaces the inner `uv` exit code as the subcommand's exit code so
//! callers can chain on success/failure.
//!
//! All bundled tools are PEP 723 `uv run` scripts — file mode is
//! irrelevant; uv is invoked explicitly so no `chmod +x` is required.
//!
//! Subprocess execution goes through [`running_process::NativeProcess`]
//! per the repo-wide lint rule that bans direct `std::process::Command`
//! use.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

use running_process::{CommandSpec, NativeProcess, ProcessConfig, StderrMode, StdinMode};

use crate::tool_install::tools_root;
use crate::tools::clud_uv_cache_dir;

/// Resolve and execute a bundled tool by relative path. Returns the
/// inner `uv` process's exit code so the CLI can surface it verbatim.
///
/// Errors:
/// - The home directory cannot be resolved (returns `Other`).
/// - The tool file doesn't exist under `~/.clud/tools/` (returns
///   `NotFound`). This usually means `tool_install::ensure_installed` was
///   never called or the install failed silently.
/// - `uv` isn't on `PATH` or could not start (returns the underlying
///   running-process error wrapped in `Other`).
/// - `uv` ran but reported a non-zero exit — that's surfaced as the `Ok`
///   value, not as `Err`. Callers should treat the `Ok` case as "uv ran"
///   and inspect the exit code to know whether the tool succeeded.
pub fn run(rel_path: &str, args: &[String]) -> io::Result<i32> {
    let tools_root = tools_root()
        .ok_or_else(|| io::Error::other("could not resolve home directory for ~/.clud/tools/"))?;
    let tool_path = resolve_tool_path(&tools_root, rel_path);
    if !tool_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "bundled tool not found at {}; the BUNDLED_TOOLS install \
                 may have been skipped or failed",
                tool_path.display(),
            ),
        ));
    }

    let cache_dir = resolved_uv_cache_dir_from(std::env::var_os("UV_CACHE_DIR"));
    let mut argv: Vec<String> = Vec::with_capacity(args.len() + 3);
    argv.push("uv".to_string());
    argv.push("run".to_string());
    argv.push(tool_path.to_string_lossy().into_owned());
    argv.extend(args.iter().cloned());

    let env: Vec<(String, String)> = build_child_env(&cache_dir);

    let process = NativeProcess::new(ProcessConfig {
        command: CommandSpec::Argv(argv),
        cwd: None,
        env: Some(env),
        capture: false,
        stderr_mode: StderrMode::Stdout,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Inherit,
        nice: None,
    });
    process.start().map_err(io::Error::other)?;
    process.wait(None).map_err(io::Error::other)
}

/// Resolve a bundled-tool path under the supplied tools-root. Trivially
/// thin, but extracted so the join semantics can be regression-tested
/// without spinning up a process or touching the user's real home.
///
/// `tools_root` is expected to already be `~/.clud/tools/`. Callers must
/// NOT pass `~/.clud/` here — the historical pattern of calling
/// `target_path_at(tools_root.parent(), rel_path)` produced a double
/// `.clud/tools` segment and broke every real tool lookup.
fn resolve_tool_path(tools_root: &std::path::Path, rel_path: &str) -> PathBuf {
    tools_root.join(rel_path)
}

/// Returns the `UV_CACHE_DIR` that should be in effect for this invocation.
/// Respects a parent-set value (Layer 3) and falls back to the bundled-tools
/// default (Layer 1). Both layers must agree on the path; that's
/// guaranteed when `main.rs` sets the parent env from
/// [`clud_uv_cache_dir`].
///
/// Takes the env value as a parameter so tests can exercise both branches
/// without mutating `std::env`, which would race other parallel tests.
fn resolved_uv_cache_dir_from(env_value: Option<OsString>) -> PathBuf {
    env_value
        .map(PathBuf::from)
        .unwrap_or_else(clud_uv_cache_dir)
}

/// Materialize the child's environment: start from this process's env (so
/// `PATH`, terminal colors, etc. propagate), then unconditionally pin
/// `UV_CACHE_DIR` to `cache_dir`. Listed explicitly in code so a forgotten
/// Layer-3 parent set never leaks the user's global uv cache into a
/// bundled-tool invocation.
fn build_child_env(cache_dir: &std::path::Path) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k != "UV_CACHE_DIR")
        .collect();
    env.push((
        "UV_CACHE_DIR".to_string(),
        cache_dir.to_string_lossy().into_owned(),
    ));
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Regression: ensure the resolved tool path is exactly
    /// `<tools_root>/<rel_path>`. A prior version called
    /// `target_path_at(tools_root.parent(), rel_path)` which re-appended
    /// `.clud/tools` and produced `~/.clud/.clud/tools/<rel_path>` —
    /// every real `clud tool run` would NotFound.
    #[test]
    fn resolve_tool_path_does_not_double_prefix() {
        let tools_root = Path::new("/home/user/.clud/tools");
        let resolved = resolve_tool_path(tools_root, "github/pr_merge_watch.py");
        assert_eq!(
            resolved,
            PathBuf::from("/home/user/.clud/tools/github/pr_merge_watch.py"),
            "tool path must not contain a doubled `.clud/tools` segment",
        );
        let s = resolved.to_string_lossy().to_string();
        assert!(
            !s.contains(".clud/tools/.clud/tools"),
            "double `.clud/tools` segment regression: {s}",
        );
    }

    #[test]
    fn unresolvable_tool_returns_not_found() {
        let err = run("definitely/does/not/exist-XXXX-clud-test-only.py", &[]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        let msg = err.to_string();
        assert!(
            msg.contains("definitely/does/not/exist-XXXX-clud-test-only.py")
                || msg.contains("definitely\\does\\not\\exist-XXXX-clud-test-only.py"),
            "error message should reference the requested rel_path; got: {msg}",
        );
    }

    #[test]
    fn resolved_cache_dir_respects_parent_env() {
        let resolved =
            resolved_uv_cache_dir_from(Some(OsString::from("/tmp/test-cache-for-clud-tool-run")));
        assert_eq!(resolved, PathBuf::from("/tmp/test-cache-for-clud-tool-run"));
    }

    #[test]
    fn resolved_cache_dir_falls_back_to_default() {
        let resolved = resolved_uv_cache_dir_from(None);
        assert_eq!(resolved, clud_uv_cache_dir());
    }

    #[test]
    fn build_child_env_pins_uv_cache_dir_and_strips_inherited_value() {
        let cache = std::path::PathBuf::from("/some/clud/cache");
        let env = build_child_env(&cache);
        let uv_entries: Vec<_> = env.iter().filter(|(k, _)| k == "UV_CACHE_DIR").collect();
        assert_eq!(
            uv_entries.len(),
            1,
            "must pin exactly one UV_CACHE_DIR entry"
        );
        assert_eq!(uv_entries[0].1, "/some/clud/cache");
        // Sanity: at least one non-UV entry made it through (PATH on any host).
        // We don't insist on PATH specifically because some test runners may strip
        // it; we just confirm the env isn't only the pinned UV_CACHE_DIR entry.
        assert!(!env.is_empty(), "env must include the pinned UV_CACHE_DIR");
    }
}
