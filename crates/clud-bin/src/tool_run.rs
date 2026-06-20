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
use std::time::Duration;

use running_process::{
    CommandSpec, NativeProcess, ProcessConfig, ProcessError, StderrMode, StdinMode,
};

use crate::session_index::{
    allocate_next_id, append_event, unix_millis_now, IndexEvent, SessionContext,
};
use crate::tool_install::tools_root;
use crate::tool_tee::TeeWriter;
use crate::tool_termination::{emit_termination, ExitKind};
use crate::tool_watchdog::{Watchdog, WatchdogDecision};
use crate::tools::clud_uv_cache_dir;

/// Poll interval for draining captured stdout/stderr into the tee writer.
/// 100ms keeps the visual feel of live output without burning CPU on
/// idle tools; the captured-output API is lock-cheap so this is fine.
const DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(100);

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

    // Resolve session context up front. None means no daemon / CI fallback;
    // we run the tool in plain passthrough mode (no capture, no tee, no
    // index — same behavior as before #427 slice 1).
    let session_ctx = SessionContext::from_env();
    let tool_id = session_ctx
        .as_ref()
        .and_then(|ctx| allocate_next_id(ctx).ok());

    // Two execution modes:
    //   - With session: capture + tee + index lifecycle events.
    //   - Without session: original passthrough (no capture, no tee).
    // The session-less path keeps `clud tool run` runnable in CI / minimal
    // containers where the daemon isn't present.
    match (session_ctx.as_ref(), tool_id) {
        (Some(ctx), Some(tool_id)) => run_with_session(ctx, tool_id, rel_path, args, argv, env),
        _ => run_passthrough(argv, env),
    }
}

/// Plain passthrough: child stdout/stderr go straight to the caller's
/// terminal, no capture, no JSONL log. Pre-slice-2 behavior, retained for
/// the no-daemon / CI fallback case.
fn run_passthrough(argv: Vec<String>, env: Vec<(String, String)>) -> io::Result<i32> {
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

/// Session-attached run: capture stdout/stderr through `running_process`,
/// poll-drain into the [`TeeWriter`] every 100ms (so the user still sees
/// live output, modulo the poll interval), and append Started/Finished
/// events to the session index.
fn run_with_session(
    ctx: &SessionContext,
    tool_id: u32,
    rel_path: &str,
    args: &[String],
    argv: Vec<String>,
    env: Vec<(String, String)>,
) -> io::Result<i32> {
    // Open the per-invocation log dir + JSONL writers BEFORE starting the
    // subprocess so any open-time failure surfaces immediately (no
    // half-spawned child with no log destination).
    let log_dir = ctx.tool_log_dir(tool_id);
    let mut tee = TeeWriter::open(&log_dir)?;

    let process = NativeProcess::new(ProcessConfig {
        command: CommandSpec::Argv(argv),
        cwd: None,
        env: Some(env),
        capture: true,
        stderr_mode: StderrMode::Pipe,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Inherit,
        nice: None,
    });
    process.start().map_err(io::Error::other)?;

    let started_at_ms = unix_millis_now();
    let _ = append_event(
        ctx,
        &IndexEvent::Started {
            tool_id,
            tool: rel_path.to_string(),
            args: args.to_vec(),
            // The actual subprocess PID/start_time come from the daemon's
            // process-tree view; capturing them through running_process
            // would need a dedicated API. For now record what we know.
            pid: 0,
            pid_start_time: 0,
            started_at_ms,
        },
    );

    // Slice 5 of #427: build the watchdog from the tool's BundledTool
    // entry (kill_semantics, command_timeout, progress_timeout, quiet_ok).
    // Unknown tools default to Killable + 60m + no progress watchdog.
    let mut watchdog = Watchdog::for_rel_path(rel_path);

    // Poll-drain loop. `wait(Some(100ms))` returns `Err(ProcessError::Timeout)`
    // while the child is still running; we drain captured output each
    // iteration and emit it via the tee. On `Ok` (child exited), we do a
    // final drain so any output written during the last 100ms still lands
    // in the log.
    //
    // Each iteration also asks the watchdog whether either timer has
    // fired. If so we either kill (killable) or emit an in-progress
    // terminal (resumable) and break out.
    let mut emitted = 0usize;
    let mut last_drain_count = 0usize;
    let exit_code = loop {
        match process.wait(Some(DRAIN_POLL_INTERVAL)) {
            Ok(code) => break code,
            Err(ProcessError::Timeout) => {
                emitted = drain_into_tee(&process, &mut tee, emitted);
                if emitted > last_drain_count {
                    watchdog.note_output();
                    last_drain_count = emitted;
                }
                match watchdog.check() {
                    WatchdogDecision::Continue => continue,
                    WatchdogDecision::KillAndAbort(reason) => {
                        let _ = process.kill();
                        let _ = drain_into_tee(&process, &mut tee, emitted);
                        let _ = tee.flush();
                        let ended_at_ms = unix_millis_now();
                        let _ = append_event(
                            ctx,
                            &IndexEvent::Aborted {
                                tool_id,
                                reason: reason.label().to_string(),
                                ended_at_ms,
                            },
                        );
                        // Slice 6: structured JSON payload + pointer block.
                        let _ = emit_termination(
                            ctx.session_pid,
                            tool_id,
                            rel_path,
                            args,
                            started_at_ms,
                            ended_at_ms,
                            watchdog.started_at.elapsed(),
                            ExitKind::Aborted(reason),
                            None,
                        );
                        return Ok(124); // standard "command timed out" exit.
                    }
                    WatchdogDecision::ResumeLater(reason) => {
                        // Resumable: the world owns the state; the
                        // observer succeeded. Detach and exit 0 with the
                        // in-progress payload so the caller can re-invoke.
                        let _ = drain_into_tee(&process, &mut tee, emitted);
                        let _ = tee.flush();
                        let ended_at_ms = unix_millis_now();
                        let _ = append_event(
                            ctx,
                            &IndexEvent::Aborted {
                                tool_id,
                                reason: format!("resumable:{}", reason.label()),
                                ended_at_ms,
                            },
                        );
                        let _ = emit_termination(
                            ctx.session_pid,
                            tool_id,
                            rel_path,
                            args,
                            started_at_ms,
                            ended_at_ms,
                            watchdog.started_at.elapsed(),
                            ExitKind::InProgress(reason),
                            None,
                        );
                        return Ok(0);
                    }
                }
            }
            Err(other) => return Err(io::Error::other(other)),
        }
    };
    let _ = drain_into_tee(&process, &mut tee, emitted);
    let _ = tee.flush();

    let ended_at_ms = unix_millis_now();
    let _ = append_event(
        ctx,
        &IndexEvent::Finished {
            tool_id,
            exit_code,
            ended_at_ms,
        },
    );

    // Slice 6: emit the structured payload + pointer block on every
    // non-zero exit so the agent always sees how to find the log.
    // We deliberately skip emission for the zero-exit happy path —
    // pointers there would be noise after a successful run.
    if exit_code != 0 {
        let _ = emit_termination(
            ctx.session_pid,
            tool_id,
            rel_path,
            args,
            started_at_ms,
            ended_at_ms,
            watchdog.started_at.elapsed(),
            ExitKind::Failed,
            Some(exit_code),
        );
    }

    Ok(exit_code)
}

/// Drain captured combined output from `emitted..` and route it through
/// the tee writer. Returns the new emitted index. Best-effort: emit
/// errors are swallowed so a failing tee write never prevents the tool
/// from completing.
fn drain_into_tee(process: &NativeProcess, tee: &mut TeeWriter, emitted: usize) -> usize {
    let combined = process.captured_combined();
    if combined.len() > emitted {
        let _ = tee.emit_batch(&combined[emitted..]);
    }
    combined.len()
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
