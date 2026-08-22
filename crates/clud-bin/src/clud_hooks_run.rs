//! Execution of declared `.clud/hooks.json` hooks (zackees/clud#967 Phase 2).
//!
//! This is "Tier B" in #966's terms: the repo's own hooks, run by clud rather
//! than by the harness. The whole point of moving them here is the rooting
//! contract:
//!
//! - **cwd is the declaring repo's root**, always, whatever the session cwd is.
//! - **`CLUD_PROJECT_DIR` names that same root**, uniformly across frontends —
//!   `$CLAUDE_PROJECT_DIR` is Claude-only and points at the *session* root, so
//!   it cannot root a sub-repo's hook.
//!
//! Together those obsolete both the broken `git rev-parse
//! --show-superproject-working-tree || ...` prefix and the cwd-drift class of
//! failure it was trying to paper over.
//!
//! ## Exit-code contract
//!
//! Mirrors the harness's, because these are the same scripts users already
//! wrote for it:
//!
//! | child exit | meaning |
//! | --- | --- |
//! | 0 | fine, continue to the next hook |
//! | 2 | block: stop, and relay the child's own output as the reason |
//! | other | the hook itself is broken — warn, continue |
//! | timeout | warn, continue |
//!
//! The last two rows fail **open** deliberately. A guard that cannot run is a
//! bug in the guard; converting it into a wall that blocks every tool call is
//! how a session wedges, which is the exact outcome this whole feature exists
//! to prevent.

use std::path::{Path, PathBuf};
use std::time::Duration;

use running_process::{
    CommandSpec, NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode,
};

use crate::clud_hooks::HookEntry;

/// Env var naming the root a hook is rooted at. Uniform across frontends,
/// unlike `$CLAUDE_PROJECT_DIR`.
pub const CLUD_PROJECT_DIR_ENV: &str = "CLUD_PROJECT_DIR";

/// What running an event's hooks decided.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookRunOutcome {
    /// Set when a hook exited 2. Carries the child's own words.
    pub deny_reason: Option<String>,
    /// Raw stdout of the denying hook, when it emitted any — a hook may
    /// speak the harness's JSON protocol, which the caller should relay
    /// verbatim rather than re-wrap.
    pub deny_stdout: Option<String>,
    /// Diagnostics for the hook log; never fatal.
    pub log_messages: Vec<String>,
}

/// Run every `entry` in order, rooted at `repo_root`, feeding each the hook
/// `payload` on stdin.
///
/// Stops at the first hook that blocks. `payload` is the raw JSON the harness
/// handed clud, forwarded unchanged so a hook sees exactly what it would have
/// seen had the harness invoked it directly.
pub fn run_hooks(entries: &[&HookEntry], repo_root: &Path, payload: &str) -> HookRunOutcome {
    let mut outcome = HookRunOutcome::default();
    for entry in entries {
        match run_one(entry, repo_root, payload) {
            Ok(result) => {
                outcome.log_messages.extend(result.log_messages);
                if result.blocked {
                    outcome.deny_reason = Some(result.reason);
                    outcome.deny_stdout = result.stdout;
                    return outcome;
                }
            }
            Err(error) => {
                // Could not even start it. Same reasoning as a non-2 exit:
                // a broken guard must not become a wall.
                let message = format!(
                    "clud_hook_spawn_failed command={:?} error={error}",
                    entry.command
                );
                eprintln!(
                    "[clud] warning: hook {:?} could not run: {error}",
                    entry.command
                );
                outcome.log_messages.push(message);
            }
        }
    }
    outcome
}

struct OneResult {
    blocked: bool,
    reason: String,
    stdout: Option<String>,
    log_messages: Vec<String>,
}

fn run_one(entry: &HookEntry, repo_root: &Path, payload: &str) -> Result<OneResult, String> {
    let config = ProcessConfig {
        // A hook command is a *shell* command line — that is the shape both
        // frontends accept and the shape users have already written, quoting
        // and all. Wrapping it in an argv would hand the shell one opaque
        // argument with its quotes intact.
        command: CommandSpec::Shell(entry.command.clone()),
        cwd: Some(repo_root.to_path_buf()),
        env: Some(hook_env(repo_root)),
        capture: true,
        // Kept separate: stdout may carry the harness's JSON protocol, while
        // stderr is the human-facing reason. Merging them would corrupt both.
        stderr_mode: StderrMode::Pipe,
        creationflags: crate::win_creation_flags::invisible_helper_creationflags(),
        create_process_group: false,
        stdin_mode: StdinMode::Piped,
        nice: None,
    };
    let process = NativeProcess::new(config);
    process
        .start()
        .map_err(|error| format!("failed to start: {error}"))?;

    // `write_stdin` closes the pipe afterwards, which is the EOF a hook
    // blocking in `json.load(sys.stdin)` is waiting for.
    if let Err(error) = process.write_stdin(payload.as_bytes()) {
        return Err(format!("failed to write payload to stdin: {error}"));
    }

    let deadline = Duration::from_secs(entry.timeout_secs);
    let mut stdout = String::new();
    let mut stderr = String::new();
    drain(&process, &mut stdout, &mut stderr, deadline);

    let exit_code = match process.wait(Some(deadline)) {
        Ok(code) => code,
        Err(error) => {
            let _ = process.kill();
            return Ok(OneResult {
                blocked: false,
                reason: String::new(),
                stdout: None,
                log_messages: vec![format!(
                    "clud_hook_timeout command={:?} after={}s error={error}",
                    entry.command, entry.timeout_secs
                )],
            });
        }
    };

    let mut log_messages = vec![format!(
        "clud_hook_ran command={:?} exit={exit_code}",
        entry.command
    )];
    if exit_code == 0 {
        return Ok(OneResult {
            blocked: false,
            reason: String::new(),
            stdout: None,
            log_messages,
        });
    }
    if exit_code != BLOCKING_EXIT_CODE {
        // Same posture as the harness: only 2 blocks. Anything else is the
        // hook being broken, and is surfaced without stopping the call.
        eprintln!(
            "[clud] warning: hook {:?} exited {exit_code} (only exit 2 blocks); continuing.{}",
            entry.command,
            trailing_detail(&stderr)
        );
        return Ok(OneResult {
            blocked: false,
            reason: String::new(),
            stdout: None,
            log_messages,
        });
    }

    log_messages.push(format!("clud_hook_blocked command={:?}", entry.command));
    let reason = blocking_reason(entry, &stderr, &stdout);
    Ok(OneResult {
        blocked: true,
        reason,
        stdout: (!stdout.trim().is_empty()).then(|| stdout.clone()),
        log_messages,
    })
}

/// The harness's blocking exit code.
const BLOCKING_EXIT_CODE: i32 = 2;

fn blocking_reason(entry: &HookEntry, stderr: &str, stdout: &str) -> String {
    // A blocking hook's message is conventionally on stderr; fall back to
    // stdout, then to naming the command, so the model never gets a bare
    // "blocked" with nothing to act on.
    for candidate in [stderr, stdout] {
        let trimmed = candidate.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    format!(
        "Blocked by the project hook {:?} (declared in .clud/hooks.json), which exited {BLOCKING_EXIT_CODE} without explaining why.",
        entry.command
    )
}

fn trailing_detail(stderr: &str) -> String {
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!(" Output: {trimmed}")
    }
}

fn drain(process: &NativeProcess, stdout: &mut String, stderr: &mut String, deadline: Duration) {
    let started = std::time::Instant::now();
    loop {
        match process.read_stream(
            running_process::StreamKind::Stdout,
            Some(Duration::from_millis(50)),
        ) {
            ReadStatus::Line(bytes) => stdout.push_str(&line_of(&bytes)),
            ReadStatus::Eof => break,
            ReadStatus::Timeout => {
                if process.returncode().is_some() || started.elapsed() >= deadline {
                    break;
                }
            }
        }
    }
    loop {
        match process.read_stream(
            running_process::StreamKind::Stderr,
            Some(Duration::from_millis(50)),
        ) {
            ReadStatus::Line(bytes) => stderr.push_str(&line_of(&bytes)),
            ReadStatus::Eof => break,
            ReadStatus::Timeout => {
                if process.returncode().is_some() || started.elapsed() >= deadline {
                    break;
                }
            }
        }
    }
}

fn line_of(raw: &[u8]) -> String {
    let mut line = String::from_utf8_lossy(raw).into_owned();
    line.push('\n');
    line
}

/// The child's environment: the current one **plus** `CLUD_PROJECT_DIR`.
///
/// `ProcessConfig.env` replaces rather than overlays, so this has to start
/// from the real environment — handing a hook an env with no `PATH` would
/// break every one of them.
fn hook_env(repo_root: &Path) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(key, _)| !key.eq_ignore_ascii_case(CLUD_PROJECT_DIR_ENV))
        .collect();
    env.push((
        CLUD_PROJECT_DIR_ENV.to_string(),
        repo_root.to_string_lossy().into_owned(),
    ));
    env
}

/// Resolve the root a declared hook should run at.
///
/// Prefers the harness's own answer when it exports one — `CLAUDE_PROJECT_DIR`
/// is the session root and is immune to cwd drift — and otherwise walks up
/// from `start`. Phase 3 replaces this with the typed root registry, where the
/// answer depends on which repo owns the touched path.
#[must_use]
pub fn resolve_root(start: &Path) -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("CLAUDE_PROJECT_DIR") {
        let root = PathBuf::from(root);
        if root.is_dir() {
            return Some(root);
        }
    }
    crate::block_bad_cmd::nearest_repo_root_public(start)
}

#[cfg(test)]
#[path = "clud_hooks_run_tests.rs"]
mod clud_hooks_run_tests;
