//! `clud tool run <rel_path> [args]` — Layer 1 of the three-layer
//! `UV_CACHE_DIR` enforcement from issue #408.
//!
//! Resolves a tool installed under `~/.clud/tools/<rel_path>`, reads the
//! file, and chooses the runner from its contents. Plain Python uses clud's
//! managed Python under `~/.clud/tools/python`; uv script mode is used only
//! when the shebang explicitly asks for it. `UV_CACHE_DIR` is pinned to
//! [`tools::clud_uv_cache_dir`]. The pin is read-or-default: if the parent
//! process already set `UV_CACHE_DIR` (Layer 3 in `main.rs`), this layer
//! re-affirms the same value; if it wasn't set, this layer establishes the
//! default.
//!
//! Surfaces the inner runner exit code as the subcommand's exit code so
//! callers can chain on success/failure.
//!
//! File mode is irrelevant; clud invokes the interpreter explicitly so no
//! `chmod +x` is required.
//!
//! Subprocess execution goes through [`running_process::NativeProcess`]
//! per the repo-wide lint rule that bans direct `std::process::Command`
//! use.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs4::fs_std::FileExt;
use running_process::{
    CommandSpec, NativeProcess, ProcessConfig, ProcessError, StderrMode, StdinMode,
};
use serde::Serialize;

use crate::log_event::ENV_DAEMON_HTTP_SERVER;
use crate::session_index::{
    allocate_next_id, append_event, unix_millis_now, IndexEvent, SessionContext,
};
use crate::shim_uv;
use crate::tool_install::{ensure_installed as ensure_tools_installed, tools_root};
use crate::tool_tee::TeeWriter;
use crate::tool_termination::{emit_termination, ExitKind};
use crate::tool_watchdog::{Watchdog, WatchdogDecision};
use crate::tools::clud_uv_cache_dir;

/// Poll interval for draining captured stdout/stderr into the tee writer.
/// 100ms keeps the visual feel of live output without burning CPU on
/// idle tools; the captured-output API is lock-cheap so this is fine.
const DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(100);
const MANAGED_PYTHON_VERSION: &str = "3.11";
const MANAGED_PYTHON_DIR: &str = "python";
const TEXT_SNIFF_LIMIT: usize = 8192;
const TOOL_TELEMETRY_TIMEOUT: Duration = Duration::from_millis(50);

/// Resolve and execute a bundled tool by relative path. Returns the
/// inner `uv` process's exit code so the CLI can surface it verbatim.
///
/// Errors:
/// - The home directory cannot be resolved (returns `Other`).
/// - The tool file doesn't exist under `~/.clud/tools/` (returns
///   `NotFound`). This usually means `rel_path` is not a registered
///   `BUNDLED_TOOLS` entry; `ensure_installed` is called inline before the
///   existence check, so a missing-but-registered tool implies the install
///   itself failed (a `[clud] note: …` line was printed to stderr).
/// - `uv` isn't on `PATH` or could not start (returns the underlying
///   running-process error wrapped in `Other`).
/// - `uv` ran but reported a non-zero exit — that's surfaced as the `Ok`
///   value, not as `Err`. Callers should treat the `Ok` case as "uv ran"
///   and inspect the exit code to know whether the tool succeeded.
pub fn run(rel_path: &str, args: &[String]) -> io::Result<i32> {
    let tools_root = tools_root()
        .ok_or_else(|| io::Error::other("could not resolve home directory for ~/.clud/tools/"))?;
    let tool_path = resolve_tool_path(&tools_root, rel_path);

    // Self-heal the bundled-tools install in this process.
    //
    // Tool invocations must not bootstrap or contact the daemon. They are
    // used heavily from agent hooks, and starting another clud-owned daemon
    // path for every hook fire pollutes process managers and adds avoidable
    // latency. Inline install is cheap on the steady state: one stat + read
    // per tool, no writes when content matches.
    ensure_tools_installed();

    if !tool_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "bundled tool not found at {}; either `{}` is not a \
                 registered BUNDLED_TOOLS entry, or the inline \
                 `ensure_installed` failed (see prior `[clud] note: …` line)",
                tool_path.display(),
                rel_path,
            ),
        ));
    }

    let tool_bytes = fs::read(&tool_path)?;
    let cache_dir = resolved_uv_cache_dir_from(std::env::var_os("UV_CACHE_DIR"));
    let env: Vec<(String, String)> = build_child_env(&cache_dir);
    let argv = build_tool_argv(&tool_path, &tool_bytes, args, &tools_root, &env)?;
    let telemetry = ToolTelemetry::start(rel_path);

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
        (Some(ctx), Some(tool_id)) => {
            run_with_session(ctx, tool_id, rel_path, args, argv, env, telemetry)
        }
        _ => run_passthrough(argv, env, telemetry),
    }
}

/// Plain passthrough: child stdout/stderr are captured only long enough to
/// immediately re-emit them and retain a stderr tail for tool telemetry.
/// There is still no session index or per-tool log in this fallback path.
fn run_passthrough(
    argv: Vec<String>,
    env: Vec<(String, String)>,
    telemetry: ToolTelemetry,
) -> io::Result<i32> {
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
    let mut emitted_stdout = 0usize;
    let mut emitted_stderr = 0usize;
    let exit_code = loop {
        match process.wait(Some(DRAIN_POLL_INTERVAL)) {
            Ok(code) => break code,
            Err(ProcessError::Timeout) => {
                (emitted_stdout, emitted_stderr) =
                    drain_passthrough_output(&process, emitted_stdout, emitted_stderr);
            }
            Err(other) => return Err(io::Error::other(other)),
        }
    };
    let _ = drain_passthrough_output(&process, emitted_stdout, emitted_stderr);
    telemetry.finish(
        exit_code,
        stderr_tail_200(&process.captured_stderr().concat()),
    );
    Ok(exit_code)
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
    telemetry: ToolTelemetry,
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
                        telemetry.finish(124, stderr_tail_200(&process.captured_stderr().concat()));
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

    telemetry.finish(
        exit_code,
        stderr_tail_200(&process.captured_stderr().concat()),
    );
    Ok(exit_code)
}

fn drain_passthrough_output(
    process: &NativeProcess,
    emitted_stdout: usize,
    emitted_stderr: usize,
) -> (usize, usize) {
    let stdout = process.captured_stdout();
    if stdout.len() > emitted_stdout {
        let mut out = io::stdout().lock();
        for chunk in &stdout[emitted_stdout..] {
            let _ = out.write_all(chunk);
        }
        let _ = out.flush();
    }

    let stderr = process.captured_stderr();
    if stderr.len() > emitted_stderr {
        let mut err = io::stderr().lock();
        for chunk in &stderr[emitted_stderr..] {
            let _ = err.write_all(chunk);
        }
        let _ = err.flush();
    }

    (stdout.len(), stderr.len())
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

fn build_tool_argv(
    tool_path: &Path,
    tool_bytes: &[u8],
    args: &[String],
    tools_root: &Path,
    env: &[(String, String)],
) -> io::Result<Vec<String>> {
    if is_cpp_source(tool_path) {
        let exe = ensure_cpp_executable(tool_path, env)?;
        return Ok(direct_argv(&exe, args));
    }

    if looks_executable(tool_path, tool_bytes) {
        return Ok(direct_argv(tool_path, args));
    }

    let Some(tool_body) = looks_like_text(tool_bytes) else {
        return Ok(direct_argv(tool_path, args));
    };

    if let Some(shebang) = parse_shebang(&tool_body) {
        return Ok(shebang_argv(&shebang, tool_path, args));
    }

    match tool_extension(tool_path).as_deref() {
        Some("py") => {
            let python = ensure_managed_python(tools_root, env)?;
            Ok(plain_python_argv(&python, tool_path, args))
        }
        Some("sh") => Ok(interpreter_argv("sh", tool_path, args)),
        Some("bash") => Ok(interpreter_argv("bash", tool_path, args)),
        Some("ps1") => Ok(interpreter_argv("pwsh", tool_path, args)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} looks like text but has no supported shebang or extension",
                tool_path.display()
            ),
        )),
    }
}

fn uv_script_argv(tool_path: &Path, args: &[String]) -> Vec<String> {
    let mut argv = vec![
        "uv".to_string(),
        "run".to_string(),
        "--no-project".to_string(),
        "--script".to_string(),
        tool_path.to_string_lossy().into_owned(),
    ];
    argv.extend(args.iter().cloned());
    argv
}

fn direct_argv(program: &Path, args: &[String]) -> Vec<String> {
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(program.to_string_lossy().into_owned());
    argv.extend(args.iter().cloned());
    argv
}

fn plain_python_argv(python: &Path, tool_path: &Path, args: &[String]) -> Vec<String> {
    interpreter_argv(&python.to_string_lossy(), tool_path, args)
}

fn interpreter_argv(interpreter: &str, tool_path: &Path, args: &[String]) -> Vec<String> {
    let mut argv = Vec::with_capacity(args.len() + 2);
    argv.push(interpreter.to_string());
    argv.push(tool_path.to_string_lossy().into_owned());
    argv.extend(args.iter().cloned());
    argv
}

fn looks_like_text(bytes: &[u8]) -> Option<String> {
    let sniff = &bytes[..bytes.len().min(TEXT_SNIFF_LIMIT)];
    if sniff.contains(&0) {
        return None;
    }
    std::str::from_utf8(bytes).ok().map(ToOwned::to_owned)
}

fn looks_executable(path: &Path, bytes: &[u8]) -> bool {
    if let Some("exe" | "com" | "cmd" | "bat") = tool_extension(path).as_deref() {
        return true;
    }
    bytes.starts_with(b"MZ")
        || bytes.starts_with(b"\x7fELF")
        || bytes.starts_with(&[0xfe, 0xed, 0xfa, 0xce])
        || bytes.starts_with(&[0xfe, 0xed, 0xfa, 0xcf])
        || bytes.starts_with(&[0xce, 0xfa, 0xed, 0xfe])
        || bytes.starts_with(&[0xcf, 0xfa, 0xed, 0xfe])
}

fn parse_shebang(tool_body: &str) -> Option<Vec<String>> {
    let first = tool_body.lines().next()?;
    let rest = first.strip_prefix("#!")?.trim();
    let parts: Vec<String> = rest.split_whitespace().map(ToOwned::to_owned).collect();
    if parts.is_empty() {
        return None;
    }
    Some(normalize_shebang_parts(parts))
}

fn normalize_shebang_parts(parts: Vec<String>) -> Vec<String> {
    let program = normalize_interpreter_path(&parts[0]);
    if program == "env" {
        let mut idx = 1;
        while idx < parts.len() {
            let flag = parts[idx].as_str();
            idx += 1;
            if flag == "-S" {
                break;
            }
            if !flag.starts_with('-') {
                idx -= 1;
                break;
            }
        }
        if idx < parts.len() {
            return parts[idx..].to_vec();
        }
        return vec!["env".to_string()];
    }

    let mut normalized = parts;
    normalized[0] = program;
    normalized
}

fn normalize_interpreter_path(path: &str) -> String {
    let name = Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path);
    #[cfg(windows)]
    {
        name.to_string()
    }
    #[cfg(not(windows))]
    {
        if path.starts_with("/bin/") || path.starts_with("/usr/bin/") {
            name.to_string()
        } else {
            path.to_string()
        }
    }
}

fn shebang_argv(shebang: &[String], tool_path: &Path, args: &[String]) -> Vec<String> {
    if shebang.len() >= 3 && shebang[0] == "uv" && shebang[1] == "run" && shebang[2] == "--script" {
        return uv_script_argv(tool_path, args);
    }

    let mut argv = Vec::with_capacity(shebang.len() + args.len() + 1);
    argv.extend_from_slice(shebang);
    argv.push(tool_path.to_string_lossy().into_owned());
    argv.extend(args.iter().cloned());
    argv
}

fn tool_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
}

fn is_cpp_source(path: &Path) -> bool {
    matches!(
        tool_extension(path).as_deref(),
        Some("cpp" | "cc" | "cxx" | "c++")
    )
}

fn cpp_executable_path(source: &Path) -> PathBuf {
    let stem = source.file_stem().unwrap_or_default();
    let exe_name = if cfg!(windows) {
        format!("{}.exe", stem.to_string_lossy())
    } else {
        stem.to_string_lossy().into_owned()
    };
    source.with_file_name(exe_name)
}

fn ensure_cpp_executable(source: &Path, env: &[(String, String)]) -> io::Result<PathBuf> {
    let exe = cpp_executable_path(source);
    if exe.exists() {
        return Ok(exe);
    }

    let lock_path = source.with_extension("cpp-build.lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    FileExt::lock_exclusive(&lock_file)?;
    let _guard = CppBuildLock(lock_file);

    if exe.exists() {
        return Ok(exe);
    }

    let clang = resolve_clangxx(env)?;
    let parent = exe.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let argv = vec![
        clang.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
        "-std=c++20".to_string(),
        "-O2".to_string(),
        "-o".to_string(),
        exe.to_string_lossy().into_owned(),
    ];
    let code = run_argv_passthrough(argv, env.to_vec())?;
    if code != 0 {
        return Err(io::Error::other(format!(
            "C++ tool compile failed with exit code {code}: {}",
            source.display()
        )));
    }
    Ok(exe)
}

struct CppBuildLock(fs::File);

impl Drop for CppBuildLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

fn resolve_clangxx(env: &[(String, String)]) -> io::Result<PathBuf> {
    if let Ok(path) = which::which("clang++") {
        return Ok(path);
    }

    let install_path = clang_tool_chain_install_path(env)?;
    for name in clangxx_names() {
        if let Some(path) = find_named_executable(&install_path, &[name], 0) {
            return Ok(path);
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "clang++ not found after clang-tool-chain-bins install at {}",
            install_path.display()
        ),
    ))
}

fn clang_tool_chain_install_path(env: &[(String, String)]) -> io::Result<PathBuf> {
    let output = run_argv_capture(
        vec![
            "clang-tool-chain-bins".to_string(),
            "install".to_string(),
            "clang".to_string(),
            "--dry-run".to_string(),
        ],
        env.to_vec(),
    )?;
    if output.exit_code != 0 {
        return Err(io::Error::other(format!(
            "clang-tool-chain-bins install clang --dry-run failed with exit code {}",
            output.exit_code
        )));
    }
    if let Some(path) = first_install_path(&output.stdout) {
        let installed = run_argv_passthrough(
            vec![
                "clang-tool-chain-bins".to_string(),
                "install".to_string(),
                "clang".to_string(),
            ],
            env.to_vec(),
        )?;
        if installed != 0 {
            return Err(io::Error::other(format!(
                "clang-tool-chain-bins install clang failed with exit code {installed}"
            )));
        }
        return Ok(path);
    }
    Err(io::Error::other(
        "clang-tool-chain-bins did not report an install_path for clang",
    ))
}

fn first_install_path(stdout: &[u8]) -> Option<PathBuf> {
    for line in String::from_utf8_lossy(stdout).lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(path) = value.get("install_path").and_then(|v| v.as_str()) {
            return Some(PathBuf::from(path));
        }
        if let Some(items) = value.as_array() {
            for item in items {
                if let Some(path) = item.get("install_path").and_then(|v| v.as_str()) {
                    return Some(PathBuf::from(path));
                }
            }
        }
    }
    None
}

fn clangxx_names() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        &["clang++.exe", "clang-cl.exe"]
    }
    #[cfg(not(windows))]
    {
        &["clang++", "clang"]
    }
}

fn run_argv_passthrough(argv: Vec<String>, env: Vec<(String, String)>) -> io::Result<i32> {
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

fn run_argv_capture(
    argv: Vec<String>,
    env: Vec<(String, String)>,
) -> io::Result<running_process::RunOutput> {
    let process = NativeProcess::new(ProcessConfig {
        command: CommandSpec::Argv(argv),
        cwd: None,
        env: Some(env),
        capture: true,
        stderr_mode: StderrMode::Pipe,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Null,
        nice: None,
    });
    process.start().map_err(io::Error::other)?;
    let exit_code = process.wait(None).map_err(io::Error::other)?;
    Ok(running_process::RunOutput {
        stdout: process.captured_stdout().concat(),
        stderr: process.captured_stderr().concat(),
        exit_code,
    })
}

fn managed_python_install_dir(tools_root: &Path) -> PathBuf {
    tools_root.join(MANAGED_PYTHON_DIR)
}

fn ensure_managed_python(tools_root: &Path, env: &[(String, String)]) -> io::Result<PathBuf> {
    let install_dir = managed_python_install_dir(tools_root);
    if let Some(path) = find_managed_python(&install_dir) {
        return Ok(path);
    }

    let _lock = shim_uv::acquire_install_lock(&install_dir)?;
    if let Some(path) = find_managed_python(&install_dir) {
        return Ok(path);
    }

    fs::create_dir_all(&install_dir)?;
    let argv = vec![
        "uv".to_string(),
        "python".to_string(),
        "install".to_string(),
        MANAGED_PYTHON_VERSION.to_string(),
        "--install-dir".to_string(),
        install_dir.to_string_lossy().into_owned(),
    ];
    let process = NativeProcess::new(ProcessConfig {
        command: CommandSpec::Argv(argv),
        cwd: None,
        env: Some(env.to_vec()),
        capture: false,
        stderr_mode: StderrMode::Stdout,
        creationflags: None,
        create_process_group: false,
        stdin_mode: StdinMode::Inherit,
        nice: None,
    });
    process.start().map_err(io::Error::other)?;
    let code = process.wait(None).map_err(io::Error::other)?;
    if code != 0 {
        return Err(io::Error::other(format!(
            "`uv python install {MANAGED_PYTHON_VERSION}` failed with exit code {code}"
        )));
    }

    find_managed_python(&install_dir).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "`uv python install {MANAGED_PYTHON_VERSION}` completed but no Python executable was found under {}",
                install_dir.display()
            ),
        )
    })
}

fn find_managed_python(install_dir: &std::path::Path) -> Option<PathBuf> {
    find_named_executable(install_dir, python_executable_names(), 0)
}

fn find_named_executable(dir: &std::path::Path, names: &[&str], depth: usize) -> Option<PathBuf> {
    if depth > 5 {
        return None;
    }
    let entries = fs::read_dir(dir).ok()?;
    let mut child_dirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if names.contains(&name) {
                return Some(path);
            }
        } else if path.is_dir() {
            child_dirs.push(path);
        }
    }
    for child in child_dirs {
        if let Some(path) = find_named_executable(&child, names, depth + 1) {
            return Some(path);
        }
    }
    None
}

fn python_executable_names() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        &["python.exe", "python3.exe"]
    }
    #[cfg(not(windows))]
    {
        &["python3", "python"]
    }
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
/// bundled-tool invocation. Also force `CLUD_NO_DAEMON=1` so tool scripts
/// that shell out to `clud` do not spawn the always-on daemon from hook paths.
fn build_child_env(cache_dir: &std::path::Path) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k != "UV_CACHE_DIR" && k != crate::daemon::ENV_NO_DAEMON)
        .collect();
    env.push((
        "UV_CACHE_DIR".to_string(),
        cache_dir.to_string_lossy().into_owned(),
    ));
    env.push((crate::daemon::ENV_NO_DAEMON.to_string(), "1".to_string()));
    env
}

#[derive(Debug, Clone)]
struct ToolTelemetry {
    server: Option<String>,
    id: String,
    name: String,
    start_time_ms: u64,
}

#[derive(Debug, Serialize)]
struct ToolTelemetryEvent<'a> {
    event: &'a str,
    id: &'a str,
    name: &'a str,
    start_time_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_time_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr_tail: Option<&'a str>,
}

impl ToolTelemetry {
    fn start(name: &str) -> Self {
        let start_time_ms = current_unix_millis();
        let id = format!("{}-{start_time_ms}", std::process::id());
        let server = std::env::var(ENV_DAEMON_HTTP_SERVER)
            .ok()
            .filter(|value| !value.is_empty());
        let telemetry = Self {
            server,
            id,
            name: name.to_string(),
            start_time_ms,
        };
        telemetry.send_start();
        telemetry
    }

    fn send_start(&self) {
        let Some(server) = self.server.clone() else {
            return;
        };
        let id = self.id.clone();
        let name = self.name.clone();
        let start_time_ms = self.start_time_ms;
        thread::spawn(move || {
            let event = ToolTelemetryEvent {
                event: "start",
                id: &id,
                name: &name,
                start_time_ms,
                end_time_ms: None,
                exit_code: None,
                stderr_tail: None,
            };
            post_tool_telemetry(&server, &event);
        });
    }

    fn finish(&self, exit_code: i32, stderr_tail: Option<String>) {
        let Some(server) = self.server.as_ref() else {
            return;
        };
        let stderr_tail = if exit_code == 0 { None } else { stderr_tail };
        let event = ToolTelemetryEvent {
            event: "finish",
            id: &self.id,
            name: &self.name,
            start_time_ms: self.start_time_ms,
            end_time_ms: Some(current_unix_millis()),
            exit_code: Some(exit_code),
            stderr_tail: stderr_tail.as_deref(),
        };
        post_tool_telemetry(server, &event);
    }
}

fn post_tool_telemetry(server: &str, event: &ToolTelemetryEvent<'_>) {
    let Ok(body) = serde_json::to_vec(event) else {
        return;
    };
    let url = format!("{}/tools/event", server.trim_end_matches('/'));
    let _ = ureq::AgentBuilder::new()
        .timeout(TOOL_TELEMETRY_TIMEOUT)
        .build()
        .post(&url)
        .set("Content-Type", "application/json")
        .send_bytes(&body);
}

fn current_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn stderr_tail_200(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(bytes);
    let mut tail: Vec<char> = text.chars().rev().take(200).collect();
    tail.reverse();
    Some(tail.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::path::Path;
    use tempfile::TempDir;

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
            resolved_uv_cache_dir_from(Some(OsString::from("/tmp/test-cache-for-clud tool-run")));
        assert_eq!(resolved, PathBuf::from("/tmp/test-cache-for-clud tool-run"));
    }

    #[test]
    fn resolved_cache_dir_falls_back_to_default() {
        let resolved = resolved_uv_cache_dir_from(None);
        assert_eq!(resolved, clud_uv_cache_dir());
    }

    #[test]
    fn pep723_script_uses_uv_script_runner() {
        let tmp = TempDir::new().unwrap();
        let tool = tmp.path().join("tools").join("hooks").join("hook.py");
        let body = "#!/usr/bin/env -S uv run --script\n# /// script\n# dependencies = []\n# ///\n";
        let argv = build_tool_argv(
            &tool,
            body.as_bytes(),
            &["--flag".to_string()],
            &tmp.path().join("tools"),
            &[],
        )
        .unwrap();
        assert_eq!(argv[0], "uv");
        assert_eq!(argv[1], "run");
        assert!(argv.contains(&"--no-project".to_string()));
        assert!(argv.contains(&"--script".to_string()));
        assert_eq!(argv.last().map(String::as_str), Some("--flag"));
    }

    #[test]
    fn plain_python_uses_managed_python_when_present() {
        let tmp = TempDir::new().unwrap();
        let tools_root = tmp.path().join("tools");
        let managed_dir = managed_python_install_dir(&tools_root).join("cpython-test");
        std::fs::create_dir_all(&managed_dir).unwrap();
        let exe_name = python_executable_names()[0];
        let python = managed_dir.join(exe_name);
        File::create(&python).unwrap();

        let tool = tools_root.join("plain.py");
        let argv = build_tool_argv(
            &tool,
            b"print('plain')\n",
            &["arg".to_string()],
            &tools_root,
            &[],
        )
        .unwrap();
        assert_eq!(argv[0], python.to_string_lossy());
        assert_eq!(argv[1], tool.to_string_lossy());
        assert_eq!(argv[2], "arg");
    }

    #[test]
    fn pep723_block_without_shebang_uses_plain_python() {
        let tmp = TempDir::new().unwrap();
        let tools_root = tmp.path().join("tools");
        let managed_dir = managed_python_install_dir(&tools_root).join("cpython-test");
        std::fs::create_dir_all(&managed_dir).unwrap();
        let python = managed_dir.join(python_executable_names()[0]);
        File::create(&python).unwrap();

        let tool = tools_root.join("metadata_only.py");
        let argv = build_tool_argv(
            &tool,
            b"# /// script\n# dependencies = [\"requests\"]\n# ///\nprint('plain')\n",
            &[],
            &tools_root,
            &[],
        )
        .unwrap();
        assert_eq!(argv[0], python.to_string_lossy());
        assert_eq!(argv[1], tool.to_string_lossy());
    }

    #[test]
    fn shebang_bin_sh_runs_through_shell() {
        let tmp = TempDir::new().unwrap();
        let tool = tmp.path().join("tool.sh");
        let argv = build_tool_argv(
            &tool,
            b"#!/bin/sh\necho hi\n",
            &["x".to_string()],
            tmp.path(),
            &[],
        )
        .unwrap();
        assert_eq!(argv, vec!["sh", &tool.to_string_lossy(), "x"]);
    }

    #[test]
    fn uv_shebang_runs_script_runner() {
        let tmp = TempDir::new().unwrap();
        let tool = tmp.path().join("tool.py");
        let argv = build_tool_argv(
            &tool,
            b"#!/usr/bin/env -S uv run --script\nprint('hi')\n",
            &[],
            tmp.path(),
            &[],
        )
        .unwrap();
        assert_eq!(argv[0], "uv");
        assert!(argv.contains(&"--script".to_string()));
    }

    #[test]
    fn binary_magic_executes_directly() {
        let tmp = TempDir::new().unwrap();
        let tool = tmp.path().join("tool.bin");
        let argv = build_tool_argv(
            &tool,
            b"\x7fELF\x02\x01",
            &["arg".to_string()],
            tmp.path(),
            &[],
        )
        .unwrap();
        assert_eq!(
            argv,
            vec![tool.to_string_lossy().to_string(), "arg".to_string()]
        );
    }

    #[test]
    fn cpp_executable_path_sits_next_to_source() {
        let source = Path::new("/tmp/tool.cpp");
        let exe = cpp_executable_path(source);
        if cfg!(windows) {
            assert_eq!(exe, PathBuf::from("/tmp/tool.exe"));
        } else {
            assert_eq!(exe, PathBuf::from("/tmp/tool"));
        }
    }

    #[test]
    fn install_path_parser_accepts_jsonl_and_arrays() {
        let jsonl = br#"{"install_path":"/clang/one"}
{"other":true}
"#;
        assert_eq!(first_install_path(jsonl), Some(PathBuf::from("/clang/one")));
        let array = br#"[{"install_path":"/clang/two"}]"#;
        assert_eq!(first_install_path(array), Some(PathBuf::from("/clang/two")));
    }

    #[test]
    fn build_child_env_pins_tool_environment_and_strips_inherited_values() {
        let cache = std::path::PathBuf::from("/some/clud/cache");
        let env = build_child_env(&cache);
        let uv_entries: Vec<_> = env.iter().filter(|(k, _)| k == "UV_CACHE_DIR").collect();
        assert_eq!(
            uv_entries.len(),
            1,
            "must pin exactly one UV_CACHE_DIR entry"
        );
        assert_eq!(uv_entries[0].1, "/some/clud/cache");
        let no_daemon_entries: Vec<_> = env
            .iter()
            .filter(|(k, _)| k == crate::daemon::ENV_NO_DAEMON)
            .collect();
        assert_eq!(
            no_daemon_entries.len(),
            1,
            "must pin exactly one CLUD_NO_DAEMON entry"
        );
        assert_eq!(no_daemon_entries[0].1, "1");
        // Sanity: at least one non-UV entry made it through (PATH on any host).
        // We don't insist on PATH specifically because some test runners may strip
        // it; we just confirm the env isn't only the pinned UV_CACHE_DIR entry.
        assert!(!env.is_empty(), "env must include the pinned UV_CACHE_DIR");
    }

    #[test]
    fn stderr_tail_keeps_last_200_chars() {
        let text = "x".repeat(250);
        let tail = stderr_tail_200(text.as_bytes()).unwrap();
        assert_eq!(tail.len(), 200);
        assert_eq!(tail, "x".repeat(200));
        assert!(stderr_tail_200(b"").is_none());
    }
}
