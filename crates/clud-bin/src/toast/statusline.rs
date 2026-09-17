//! Toasts through Claude Code's own status line (#1189).
//!
//! In subprocess mode clud never sees the child's output, so it cannot draw
//! into the terminal grid. Claude Code, however, renders a `statusLine`
//! command's output in its own footer, anchored and never overlapping. clud
//! injects a `statusLine` that runs `clud statusline`, which:
//!
//! 1. runs the user's own status-line command (if any) with the same stdin
//!    and prints its output first, so the user's line is never replaced;
//! 2. appends clud's live toast, read from a small per-session state file
//!    that [`StatusStateWriter`] keeps current.
//!
//! Expiry is data, not a timer: the file carries `expires_ms`, and a toast
//! whose writer died is ignored once `updated_ms` goes stale.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use running_process::{
    CommandSpec, NativeProcess, ProcessConfig, ReadStatus, StderrMode, StdinMode, StreamKind,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Severity, Toast, ToastBoard, ToastEvent};

pub const SUBCOMMAND: &str = "statusline";

/// Claude Code's minimum `refreshInterval`-scale cadence that keeps an idle
/// session's toast current and lets expiry show within ~2 s.
pub const REFRESH_INTERVAL_SECS: u64 = 2;

/// A non-expiring toast whose writer has not refreshed it for this long is
/// treated as orphaned (its session died without cleaning up).
pub const STALE_AFTER: Duration = Duration::from_secs(90);

/// Upper bound on the user's chained status-line command.
const CHAIN_DEADLINE: Duration = Duration::from_secs(5);

/// Marker that identifies a status-line command clud generated, so a
/// discovered user command that is already clud's is never chained into
/// itself.
const MARKER: &str = " statusline --session-pid ";

pub fn state_path(state_dir: &Path, session_pid: u32) -> PathBuf {
    state_dir.join("toasts").join(format!("{session_pid}.json"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusState {
    pub updated_ms: u64,
    pub toast: Option<StatusToast>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusToast {
    pub text: String,
    pub severity: Severity,
    pub expires_ms: Option<u64>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Keeps a session's state file in step with its toasts. Removes the file on
/// drop so a finished session never leaves a toast behind.
#[derive(Debug)]
pub struct StatusStateWriter {
    path: PathBuf,
    board: Mutex<ToastBoard>,
}

impl StatusStateWriter {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            board: Mutex::new(ToastBoard::default()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Apply `event` and rewrite the file. Every publish refreshes
    /// `updated_ms`, which is the heartbeat that keeps a live toast fresh.
    pub fn publish(&self, event: ToastEvent) {
        let now = Instant::now();
        let visible = {
            let mut board = self.board.lock().unwrap_or_else(|e| e.into_inner());
            board.apply(event);
            board.expire(now);
            board.visible(now).cloned()
        };
        let _ = self.write(visible.as_ref(), now);
    }

    fn write(&self, visible: Option<&Toast>, now: Instant) -> io::Result<()> {
        let wall = now_ms();
        let state = StatusState {
            updated_ms: wall,
            toast: visible.map(|toast| StatusToast {
                text: toast.text.clone(),
                severity: toast.severity,
                expires_ms: toast.expires_at.map(|at| {
                    wall.saturating_add(
                        u64::try_from(at.saturating_duration_since(now).as_millis())
                            .unwrap_or(u64::MAX),
                    )
                }),
            }),
        };
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&state).map_err(io::Error::other)?)?;
        std::fs::rename(&tmp, &self.path)
    }
}

impl Drop for StatusStateWriter {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The toast to show at `now_ms`, or `None` when absent, expired, or orphaned.
pub fn read_live_toast(path: &Path, now_ms: u64) -> Option<StatusToast> {
    let bytes = std::fs::read(path).ok()?;
    let state: StatusState = serde_json::from_slice(&bytes).ok()?;
    let toast = state.toast?;
    match toast.expires_ms {
        Some(expires) if now_ms >= expires => None,
        Some(_) => Some(toast),
        None => {
            let stale_ms = u64::try_from(STALE_AFTER.as_millis()).unwrap_or(u64::MAX);
            (now_ms.saturating_sub(state.updated_ms) <= stale_ms).then_some(toast)
        }
    }
}

pub fn render_line(toast: &StatusToast) -> String {
    let colour = match toast.severity {
        Severity::Info => "\x1b[38;5;75m",
        Severity::Warn => "\x1b[38;5;214m",
        Severity::Alert => "\x1b[1;38;5;203m",
    };
    let text: String = toast.text.chars().filter(|c| !c.is_control()).collect();
    format!("{colour}clud \u{b7} {text}\x1b[0m")
}

/// The `statusLine.command` string Claude Code runs. `None` when a path would
/// need quoting we cannot express portably. On Windows paths use forward
/// slashes, which both Git Bash and `cmd` accept.
pub fn statusline_command(
    exe: &Path,
    session_pid: u32,
    state_dir: &Path,
    chain: Option<&str>,
    windows: bool,
) -> Option<String> {
    let render = |path: &Path| {
        let text = path.to_string_lossy().into_owned();
        if windows {
            crate::path_norm::slash_separators(&text)
        } else {
            text
        }
    };
    let (exe, dir) = (render(exe), render(state_dir));
    if [&exe, &dir]
        .iter()
        .any(|s| s.contains('"') || s.contains('$') || s.contains('`'))
    {
        return None;
    }
    let mut command = format!("\"{exe}\"{MARKER}{session_pid} --state-dir \"{dir}\"");
    if let Some(chain) = chain {
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(chain);
        command.push_str(&format!(" --chain-b64 {encoded}"));
    }
    Some(command)
}

pub fn decode_chain(encoded: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded.trim())
        .ok()?;
    String::from_utf8(bytes).ok()
}

/// The user command to chain from a discovered `statusLine` setting.
pub fn chained_command(user: Option<&Value>) -> Option<String> {
    let user = user?;
    if user
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("command")
        != "command"
    {
        return None;
    }
    let command = user.get("command").and_then(Value::as_str)?.trim();
    (!command.is_empty() && !command.contains(MARKER)).then(|| command.to_string())
}

/// clud's `statusLine` value, keeping the user's other fields (padding and
/// friends) and never slowing a faster refresh the user already chose.
pub fn compose_setting(user: Option<&Value>, command: String) -> Value {
    let mut object = user.and_then(Value::as_object).cloned().unwrap_or_default();
    let refresh = object
        .get("refreshInterval")
        .and_then(Value::as_u64)
        .filter(|r| *r > 0)
        .map_or(REFRESH_INTERVAL_SECS, |r| r.min(REFRESH_INTERVAL_SECS));
    object.insert("type".into(), Value::from("command"));
    object.insert("command".into(), Value::from(command));
    object.insert("refreshInterval".into(), Value::from(refresh));
    Value::Object(object)
}

/// The user's effective `statusLine`, by Claude Code's precedence: an explicit
/// `--settings` document, then the project's `settings.local.json` and
/// `settings.json`, then `~/.claude/settings.json`.
pub fn discover_user_statusline(
    explicit_settings: Option<&Value>,
    project_dir: &Path,
    home: Option<&Path>,
) -> Option<Value> {
    if let Some(value) = explicit_settings.and_then(|s| s.get("statusLine")) {
        return Some(value.clone());
    }
    let home_claude = home.map(|h| h.join(".claude"));
    for dir in project_dir.ancestors() {
        let claude = dir.join(".claude");
        if home_claude.as_deref() == Some(claude.as_path()) {
            break;
        }
        let local = claude.join("settings.local.json");
        let shared = claude.join("settings.json");
        if !local.is_file() && !shared.is_file() {
            continue;
        }
        if let Some(found) = read_statusline(&local).or_else(|| read_statusline(&shared)) {
            return Some(found);
        }
        break;
    }
    home_claude.and_then(|dir| read_statusline(&dir.join("settings.json")))
}

fn read_statusline(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value.get("statusLine").cloned()
}

/// Arguments of the hidden `clud statusline` subcommand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunArgs {
    pub session_pid: u32,
    pub state_dir: PathBuf,
    pub chain_b64: Option<String>,
}

/// Entry point for `clud statusline`. Always exits 0: a failing status line
/// must never disturb the session that renders it.
pub fn run(args: &RunArgs) -> i32 {
    let mut input = Vec::new();
    let _ = io::stdin().read_to_end(&mut input);
    let mut out = Vec::new();
    render_into(&mut out, args, &input, now_ms());
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(&out);
    let _ = stdout.flush();
    0
}

/// Everything `run` prints, separated from stdio for tests.
pub fn render_into(out: &mut Vec<u8>, args: &RunArgs, stdin: &[u8], now_ms: u64) {
    if let Some(chain) = args.chain_b64.as_deref().and_then(decode_chain) {
        if let Some(user) = run_chain(&chain, stdin) {
            let trimmed = trim_trailing_newlines(&user);
            if !trimmed.is_empty() {
                out.extend_from_slice(trimmed);
                out.push(b'\n');
            }
        }
    }
    if let Some(toast) = read_live_toast(&state_path(&args.state_dir, args.session_pid), now_ms) {
        out.extend_from_slice(render_line(&toast).as_bytes());
        out.push(b'\n');
    }
}

fn trim_trailing_newlines(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 && matches!(bytes[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    &bytes[..end]
}

/// Run the user's status-line command with Claude's session JSON on stdin and
/// return its stdout. Stderr is captured separately and dropped, as Claude
/// Code itself ignores a status line's stderr.
pub fn run_chain(command: &str, stdin: &[u8]) -> Option<Vec<u8>> {
    let config = ProcessConfig {
        command: chain_spec(command),
        cwd: None,
        env: None,
        capture: true,
        stderr_mode: StderrMode::Pipe,
        creationflags: crate::win_creation_flags::invisible_helper_creationflags(),
        create_process_group: false,
        stdin_mode: StdinMode::Piped,
        nice: None,
    };
    let process = NativeProcess::new(config);
    process.start().ok()?;
    let _ = process.write_stdin(stdin);
    let started = Instant::now();
    let mut buf = Vec::new();
    loop {
        match process.read_combined(Some(Duration::from_millis(50))) {
            ReadStatus::Line(event) => {
                if event.stream == StreamKind::Stdout {
                    buf.extend_from_slice(&event.line);
                    buf.push(b'\n');
                }
            }
            ReadStatus::Timeout => {
                if process.returncode().is_some() {
                    break;
                }
                if started.elapsed() >= CHAIN_DEADLINE {
                    let _ = process.kill();
                    break;
                }
            }
            ReadStatus::Eof => break,
        }
    }
    let _ = process.wait(Some(Duration::from_secs(2)));
    Some(buf)
}

/// POSIX runs the command with `sh -c`, as Claude Code does. Windows prefers
/// Git Bash (Claude Code's own shell there) and falls back to `cmd`.
fn chain_spec(command: &str) -> CommandSpec {
    #[cfg(windows)]
    {
        let bash = std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH")
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .or_else(|| which::which("bash").ok());
        match bash {
            Some(bash) => CommandSpec::Argv(vec![
                bash.to_string_lossy().into_owned(),
                "-c".into(),
                command.into(),
            ]),
            None => CommandSpec::Shell(command.into()),
        }
    }
    #[cfg(not(windows))]
    {
        CommandSpec::Argv(vec!["sh".into(), "-c".into(), command.into()])
    }
}

#[cfg(test)]
#[path = "statusline_tests.rs"]
mod tests;
