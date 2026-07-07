//! Native `block-bad-cmd` PreToolUse hook.
//!
//! The hot path is a dedicated Rust binary (`clud-block-bad-cmd`) so hook
//! fires do not launch Python or uv.

use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const STDIN_READ_CHUNK_BYTES: usize = 64 * 1024;
pub const STDIN_READ_MAX_BYTES: usize = 1024 * 1024;
const DEFAULT_STDIN_READ_IDLE_TIMEOUT_SEC: f64 = 0.25;
const DEFAULT_STDIN_READ_DEADLINE_SEC: f64 = 2.0;
const LOG_REL_PATH: &str = ".clud/tools/hooks/block-bad-cmd.log";
const SENTINEL_PHRASE: &str = concat!("bad", " cmd");

const TOOL_RS_BUILD: &str = concat!("car", "go");
const TOOL_RS_COMPILER: &str = concat!("rust", "c");
const TOOL_RS_FORMAT: &str = concat!("rust", "fmt");
const TOOL_RS_RUNNER: &str = concat!("rust", "up");

const RUST_TOOLS: &[&str] = &[
    TOOL_RS_BUILD,
    TOOL_RS_COMPILER,
    TOOL_RS_FORMAT,
    concat!("clippy", "-driver"),
    concat!("car", "go", "-clippy"),
    concat!("car", "go", "-fmt"),
    TOOL_RS_RUNNER,
    concat!("rust", "doc"),
    concat!("rust", "-gdb"),
    concat!("rust", "-lldb"),
    concat!("rust", "-analyzer"),
];

const LEGACY_RUST_TRAMPOLINES: &[&str] = &[
    concat!("_car", "go"),
    concat!("_rust", "c"),
    concat!("_rust", "fmt"),
];
const SHELL_WRAPPERS: &[&str] = &["cmd", "powershell", "pwsh", "bash", "sh", "zsh"];

const UV_RUN_OPTIONS_WITH_VALUE: &[&str] = &[
    "--allow-insecure-host",
    "--cache-dir",
    "--color",
    "--config-setting",
    "--config-settings-package",
    "--config-file",
    "--default-index",
    "--directory",
    "--env-file",
    "--exclude-newer-package",
    "--exclude-newer",
    "--extra",
    "--extra-index-url",
    "--find-links",
    "--fork-strategy",
    "--group",
    "--gui-script",
    "--index",
    "--index-url",
    "--index-strategy",
    "--keyring-provider",
    "--link-mode",
    "--module",
    "--no-binary-package",
    "--no-build-isolation-package",
    "--no-build-package",
    "--no-editable-package",
    "--no-extra",
    "--no-group",
    "--no-sources-package",
    "--only-group",
    "--package",
    "--prerelease",
    "--project",
    "--python",
    "--python-platform",
    "--refresh-package",
    "--reinstall-package",
    "--resolution",
    "--script",
    "--upgrade-group",
    "--upgrade-package",
    "--with",
    "--with-editable",
    "--with-requirements",
];
const UV_RUN_SHORT_OPTIONS_WITH_VALUE: &[&str] = &["-C", "-P", "-f", "-i", "-m", "-p", "-s", "-w"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookPayloadView {
    pub tool_name: String,
    pub command: String,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny { reason: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandEvaluation {
    pub reason: Option<String>,
    pub warnings: Vec<String>,
    pub log_messages: Vec<String>,
}

#[derive(Debug, Clone)]
struct StdinRead {
    text: String,
    log_messages: Vec<String>,
}

pub fn run() -> i32 {
    let stdin = read_stdin_bounded();
    for message in &stdin.log_messages {
        append_log(message);
    }
    append_log(&format!("raw_stdin_bytes={}", stdin.text.len()));

    let payload: Value = match serde_json::from_str(if stdin.text.trim().is_empty() {
        "{}"
    } else {
        &stdin.text
    }) {
        Ok(value) => value,
        Err(error) => {
            append_log(&format!("json_decode_error: {error}"));
            return 0;
        }
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(payload) = parse_payload_value(&payload, &cwd) else {
        append_log("unsupported_payload_shape");
        return 0;
    };
    append_log(&format!(
        "tool_name={:?} cwd={:?} command={:?}",
        payload.tool_name,
        payload.cwd.to_string_lossy(),
        payload.command
    ));

    let allow_hybrid_uv_run = std::env::var("CLUD_UV_RUST_ALLOW_ALL").ok().as_deref() == Some("1");
    let evaluation = evaluate_command(&payload.command, Some(&payload.cwd), allow_hybrid_uv_run);
    for message in &evaluation.log_messages {
        append_log(message);
    }
    for warning in &evaluation.warnings {
        eprintln!("{warning}");
    }

    if let Some(reason) = evaluation.reason {
        let msg = format!(
            "[block-bad-cmd hook] refusing to run {:?}: {reason}",
            payload.tool_name
        );
        append_log(&format!("BLOCKED: {msg}"));
        println!("{}", deny_json(&reason));
        eprintln!("{msg}");
        return 2;
    }

    append_log("allowed");
    0
}

pub fn parse_payload(raw: &str, process_cwd: &Path) -> Option<HookPayloadView> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    parse_payload_value(&value, process_cwd)
}

pub fn parse_payload_value(value: &Value, process_cwd: &Path) -> Option<HookPayloadView> {
    let object = value.as_object()?;
    let tool_name = object
        .get("tool_name")
        .or_else(|| object.get("toolName"))
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let command = extract_command(value);
    let cwd = object
        .get("cwd")
        .or_else(|| object.get("cwdPath"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| process_cwd.to_path_buf());
    Some(HookPayloadView {
        tool_name,
        command,
        cwd,
    })
}

pub fn forbidden_reason(command_text: &str, cwd: Option<&Path>) -> Option<String> {
    let allow_hybrid_uv_run = std::env::var("CLUD_UV_RUST_ALLOW_ALL").ok().as_deref() == Some("1");
    evaluate_command(command_text, cwd, allow_hybrid_uv_run).reason
}

pub fn decision_from_payload(payload: &HookPayloadView) -> Decision {
    match forbidden_reason(&payload.command, Some(&payload.cwd)) {
        Some(reason) => Decision::Deny { reason },
        None => Decision::Allow,
    }
}

pub fn deny_json(reason: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
}

pub fn evaluate_command(
    command_text: &str,
    cwd: Option<&Path>,
    allow_hybrid_uv_run: bool,
) -> CommandEvaluation {
    let mut evaluation = CommandEvaluation::default();
    evaluate_command_into(command_text, cwd, allow_hybrid_uv_run, &mut evaluation);
    evaluation
}

fn evaluate_command_into(
    command_text: &str,
    cwd: Option<&Path>,
    allow_hybrid_uv_run: bool,
    evaluation: &mut CommandEvaluation,
) {
    if command_text.to_ascii_lowercase().contains(SENTINEL_PHRASE) {
        evaluation.reason = Some(format!(
            "command contains {:?}. Full command: {}",
            SENTINEL_PHRASE,
            py_string_repr(command_text)
        ));
        return;
    }

    for segment in split_shell_segments(command_text) {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        let words = command_words(segment);
        if words.is_empty() {
            continue;
        }

        let first = program_name(&words[0]);
        if let Some(nested) = nested_shell_command(&words) {
            evaluate_command_into(&nested, cwd, allow_hybrid_uv_run, evaluation);
            if evaluation.reason.is_some() {
                return;
            }
            continue;
        }

        if contains_str(LEGACY_RUST_TRAMPOLINES, &first) {
            evaluation.reason = Some(format!(
                "Use `soldr {} ...` instead of legacy `{}`. The root Rust trampolines bypass soldr's toolchain selection.",
                first.trim_start_matches('_'),
                words[0]
            ));
            return;
        }

        if first == "soldr" {
            continue;
        }

        if first == "uv" && words.len() > 1 && words[1] == "run" {
            if let Some(tool) = resolve_uv_run_tool(&words) {
                let tool_bare = program_name(&tool);
                if contains_str(LEGACY_RUST_TRAMPOLINES, &tool_bare) {
                    evaluation.reason = Some(format!(
                        "Use `soldr {} ...` instead of legacy `{}`. The root Rust trampolines bypass soldr's toolchain selection.",
                        tool_bare.trim_start_matches('_'),
                        tool
                    ));
                    return;
                }
                if contains_str(RUST_TOOLS, &tool_bare) {
                    evaluation.reason = Some(format!(
                        "Use `soldr {tool_bare} ...` instead of `uv run {tool} ...`. `uv run <rust-tool>` bypasses soldr's toolchain selection."
                    ));
                    return;
                }
            }

            let uv_safe_flags = ["--no-project", "--no-sync", "--frozen"];
            let has_uv_safe_flag = words[2..].iter().any(|word| {
                uv_safe_flags
                    .iter()
                    .any(|flag| word == flag || word.starts_with(&format!("{flag}=")))
            });
            if !has_uv_safe_flag {
                if let Some(hybrid_root) = python_rust_hybrid_root(cwd) {
                    if allow_hybrid_uv_run {
                        evaluation.log_messages.push(format!(
                            "CLUD_UV_RUST_ALLOW_ALL=1 bypassed hybrid block at {}",
                            hybrid_root.display()
                        ));
                        evaluation
                            .warnings
                            .push(hybrid_bypass_warning(&hybrid_root));
                    } else {
                        evaluation.reason = Some(format!(
                            "this hook fired because {} contains both pyproject.toml and Cargo.toml (a Python+Rust hybrid project). `uv run` without --no-project / --no-sync / --frozen triggers the project auto-sync, which on a Rust-backed wheel is a full native rebuild. Pass `--no-project` for pure-Python scripts, `--no-sync` to use the existing venv, or `--frozen` to lock to the existing lockfile. Escape hatch for a legitimate full-rebuild: run `./test` (or `bash ./test`) - the canonical full-build entrypoint. Set CLUD_UV_RUST_ALLOW_ALL=1 to bypass this gate with a warning. See zackees/soldr#805.",
                            hybrid_root.display()
                        ));
                        return;
                    }
                }
            }
            continue;
        }

        if contains_str(RUST_TOOLS, &first) {
            evaluation.reason = Some(format!(
                "Use `soldr {first} ...` instead of bare `{first}`. soldr resolves the pinned rustup-managed toolchain and avoids GNU/Chocolatey shims."
            ));
            return;
        }
    }
}

fn extract_command(payload: &Value) -> String {
    let Some(object) = payload.as_object() else {
        return String::new();
    };
    let Some(tool_input) = object.get("tool_input").or_else(|| object.get("toolInput")) else {
        return String::new();
    };
    if let Some(map) = tool_input.as_object() {
        for key in ["command", "script"] {
            if let Some(command) = map.get(key).and_then(Value::as_str) {
                return command.to_string();
            }
        }
        if let Some(argv) = map.get("argv").and_then(Value::as_array) {
            return argv
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(ToOwned::to_owned)
                        .unwrap_or_else(|| value.to_string())
                })
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
    tool_input.as_str().unwrap_or("").to_string()
}

fn split_shell_segments(command_text: &str) -> Vec<String> {
    let chars = command_text.chars().collect::<Vec<_>>();
    let mut segments = Vec::new();
    let mut buf = String::new();
    let mut quote: Option<char> = None;
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if let Some(q) = quote {
            buf.push(ch);
            if ch == q {
                quote = None;
            }
            i += 1;
            continue;
        }

        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            buf.push(ch);
            i += 1;
            continue;
        }

        let is_double_amp = ch == '&' && i + 1 < chars.len() && chars[i + 1] == '&';
        let is_double_pipe = ch == '|' && i + 1 < chars.len() && chars[i + 1] == '|';
        if matches!(ch, ';' | '|' | '\r' | '\n') || is_double_amp {
            let segment = buf.trim();
            if !segment.is_empty() {
                segments.push(segment.to_string());
            }
            buf.clear();
            i += if is_double_amp || is_double_pipe {
                2
            } else {
                1
            };
            continue;
        }

        buf.push(ch);
        i += 1;
    }

    let segment = buf.trim();
    if !segment.is_empty() {
        segments.push(segment.to_string());
    }
    segments
}

fn tokenize(segment: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut buf = String::new();
    let mut quote: Option<char> = None;
    for ch in segment.chars() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                buf.push(ch);
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            continue;
        }
        if ch.is_whitespace() {
            if !buf.is_empty() {
                words.push(std::mem::take(&mut buf));
            }
            continue;
        }
        buf.push(ch);
    }
    if !buf.is_empty() {
        words.push(buf);
    }
    words
}

fn program_name(word: &str) -> String {
    let cleaned = word.trim().trim_matches(&['\'', '"'][..]);
    crate::path_norm::file_stem_any_separator(cleaned)
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn command_words(segment: &str) -> Vec<String> {
    let mut words = tokenize(segment);
    while words
        .first()
        .is_some_and(|word| ["&", "call", "exec", "command"].contains(&word.as_str()))
    {
        words.remove(0);
    }
    if words
        .first()
        .is_some_and(|word| program_name(word) == "env")
    {
        words.remove(0);
    }
    while words.first().is_some_and(|word| is_env_assignment(word)) {
        words.remove(0);
    }
    words
}

fn is_env_assignment(word: &str) -> bool {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    for ch in chars {
        if ch == '=' {
            return true;
        }
        if !(ch == '_' || ch.is_ascii_alphanumeric()) {
            return false;
        }
    }
    false
}

fn resolve_uv_run_tool(words: &[String]) -> Option<String> {
    if words.len() < 3 || program_name(&words[0]) != "uv" || words[1] != "run" {
        return None;
    }
    let mut i = 2usize;
    while i < words.len() {
        let word = &words[i];
        if word == "--" {
            i += 1;
            break;
        }
        if word == "--script" && i + 1 < words.len() {
            return Some(words[i + 1].clone());
        }
        if let Some(value) = word.strip_prefix("--script=") {
            return Some(value.to_string());
        }
        if !word.starts_with('-') {
            break;
        }
        let consumes_value = (!word.contains('=') && contains_str(UV_RUN_OPTIONS_WITH_VALUE, word))
            || contains_str(UV_RUN_SHORT_OPTIONS_WITH_VALUE, word);
        if consumes_value {
            i += 2;
        } else {
            i += 1;
        }
    }
    words.get(i).cloned()
}

fn nested_shell_command(words: &[String]) -> Option<String> {
    let first = program_name(words.first()?);
    if !contains_str(SHELL_WRAPPERS, &first) {
        return None;
    }
    if first == "cmd" {
        for (i, word) in words.iter().enumerate().skip(1) {
            if ["/c", "/r"].contains(&word.to_ascii_lowercase().as_str()) && i + 1 < words.len() {
                return Some(words[i + 1..].join(" "));
            }
        }
        return None;
    }
    if first == "powershell" || first == "pwsh" {
        for (i, word) in words.iter().enumerate().skip(1) {
            if ["-command", "-c", "/c"].contains(&word.to_ascii_lowercase().as_str())
                && i + 1 < words.len()
            {
                return Some(words[i + 1..].join(" "));
            }
        }
        return None;
    }

    for (i, word) in words.iter().enumerate().skip(1) {
        let option = word.to_ascii_lowercase();
        let option = option.trim_start_matches('-');
        if option.contains('c') && i + 1 < words.len() {
            return Some(words[i + 1..].join(" "));
        }
    }
    None
}

fn python_rust_hybrid_root(cwd: Option<&Path>) -> Option<PathBuf> {
    let anchor = cwd?.canonicalize().ok()?;
    for candidate in std::iter::once(anchor.as_path()).chain(anchor.ancestors().skip(1)) {
        if candidate.join("pyproject.toml").is_file() && candidate.join("Cargo.toml").is_file() {
            return Some(candidate.to_path_buf());
        }
    }
    None
}

fn hybrid_bypass_warning(hybrid_root: &Path) -> String {
    format!(
        "\x1b[33mWARNING: AUTO COMPILING RUST because of uv run\n\
CLUD_UV_RUST_ALLOW_ALL=1 is set, so the auto-sync gate at {} was bypassed.\n\
DIRECTIVE TO AGENT: the next `uv run` in this project root will trigger a full native rebuild (can take minutes). \
If you don't need a fresh build, pass `--no-sync` (use existing venv), `--no-project` (pure-Python script), or \
`--frozen` (lock to existing lockfile) to skip the auto-sync. If you DO need a clean rebuild, prefer `./test` \
(or `bash ./test`) - the canonical full-build entrypoint.\x1b[0m",
        hybrid_root.display()
    )
}

fn contains_str(haystack: &[&str], needle: &str) -> bool {
    haystack.iter().any(|item| item == &needle)
}

fn py_string_repr(value: &str) -> String {
    let mut out = String::from("'");
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('\'');
    out
}

fn read_stdin_bounded() -> StdinRead {
    #[cfg(unix)]
    {
        if let Some(read) = read_stdin_nonblocking() {
            return read;
        }
    }
    read_stdin_threaded()
}

#[cfg(unix)]
fn read_stdin_nonblocking() -> Option<StdinRead> {
    use std::os::fd::AsRawFd;

    let idle_timeout = float_env_duration(
        "CLUD_HOOK_STDIN_IDLE_TIMEOUT_SEC",
        DEFAULT_STDIN_READ_IDLE_TIMEOUT_SEC,
    );
    let deadline_timeout = float_env_duration(
        "CLUD_HOOK_STDIN_DEADLINE_SEC",
        DEFAULT_STDIN_READ_DEADLINE_SEC,
    );

    let stdin = io::stdin();
    let mut stream = stdin.lock();
    let fd = stream.as_raw_fd();
    let old_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if old_flags < 0 {
        return None;
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, old_flags | libc::O_NONBLOCK) } < 0 {
        return None;
    }

    let mut chunks = Vec::<u8>::new();
    let mut log_messages = Vec::<String>::new();
    let deadline = Instant::now() + deadline_timeout;
    let mut idle_until: Option<Instant> = None;
    let mut incomplete_reason: Option<&'static str> = None;
    loop {
        let mut buf = [0u8; STDIN_READ_CHUNK_BYTES];
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                chunks.extend_from_slice(&buf[..n]);
                idle_until = Some(Instant::now() + idle_timeout);
                if chunks.len() >= STDIN_READ_MAX_BYTES {
                    incomplete_reason = Some("max_bytes");
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let now = Instant::now();
                let wait_until = idle_until.map_or(deadline, |idle| idle.min(deadline));
                if now >= wait_until {
                    incomplete_reason = Some(if idle_until.is_some() && wait_until <= deadline {
                        "idle"
                    } else {
                        "deadline"
                    });
                    break;
                }
                std::thread::sleep((wait_until - now).min(Duration::from_millis(10)));
            }
            Err(error) => {
                log_messages.push(format!("stdin_read_error mode=nonblocking error={error}"));
                break;
            }
        }
    }

    let _ = unsafe { libc::fcntl(fd, libc::F_SETFL, old_flags) };
    if let Some(reason) = incomplete_reason {
        log_messages.push(format!(
            "stdin_read_incomplete mode=nonblocking reason={reason} bytes={}",
            chunks.len()
        ));
    }
    Some(StdinRead {
        text: decode_stdin(&chunks),
        log_messages,
    })
}

fn read_stdin_threaded() -> StdinRead {
    enum Item {
        Chunk(Vec<u8>),
        Eof,
        Error(String),
    }

    let idle_timeout = float_env_duration(
        "CLUD_HOOK_STDIN_IDLE_TIMEOUT_SEC",
        DEFAULT_STDIN_READ_IDLE_TIMEOUT_SEC,
    );
    let deadline_timeout = float_env_duration(
        "CLUD_HOOK_STDIN_DEADLINE_SEC",
        DEFAULT_STDIN_READ_DEADLINE_SEC,
    );
    let (tx, rx) = mpsc::channel::<Item>();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut stream = stdin.lock();
        loop {
            let mut buf = vec![0u8; STDIN_READ_CHUNK_BYTES];
            match stream.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(Item::Eof);
                    return;
                }
                Ok(n) => {
                    buf.truncate(n);
                    if tx.send(Item::Chunk(buf)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = tx.send(Item::Error(error.to_string()));
                    return;
                }
            }
        }
    });

    let mut chunks = Vec::<u8>::new();
    let mut log_messages = Vec::<String>::new();
    let deadline = Instant::now() + deadline_timeout;
    let mut idle_until: Option<Instant> = None;
    let mut incomplete_reason: Option<&'static str> = None;
    loop {
        let now = Instant::now();
        let wait_until = idle_until.map_or(deadline, |idle| idle.min(deadline));
        if now >= wait_until {
            incomplete_reason = Some(if idle_until.is_some() && wait_until <= deadline {
                "idle"
            } else {
                "deadline"
            });
            break;
        }
        match rx.recv_timeout(wait_until - now) {
            Ok(Item::Eof) => break,
            Ok(Item::Error(error)) => {
                log_messages.push(format!("stdin_read_error mode=threaded error={error}"));
                break;
            }
            Ok(Item::Chunk(chunk)) => {
                chunks.extend_from_slice(&chunk);
                idle_until = Some(Instant::now() + idle_timeout);
                if chunks.len() >= STDIN_READ_MAX_BYTES {
                    incomplete_reason = Some("max_bytes");
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                incomplete_reason = Some(if idle_until.is_some() && wait_until <= deadline {
                    "idle"
                } else {
                    "deadline"
                });
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    if let Some(reason) = incomplete_reason {
        log_messages.push(format!(
            "stdin_read_incomplete mode=threaded reason={reason} bytes={}",
            chunks.len()
        ));
    }
    StdinRead {
        text: decode_stdin(&chunks),
        log_messages,
    }
}

fn decode_stdin(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_start_matches('\u{feff}')
        .to_string()
}

fn float_env_duration(name: &str, default: f64) -> Duration {
    let seconds = std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<f64>().ok())
        .unwrap_or(default)
        .max(0.01);
    Duration::from_secs_f64(seconds)
}

pub fn log_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(LOG_REL_PATH))
}

fn append_log(message: &str) {
    let Some(path) = log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string());
    let _ = writeln!(file, "[{timestamp}] pid={} {message}", std::process::id());
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(path) = std::env::var_os("USERPROFILE") {
            if !path.to_string_lossy().is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }
    std::env::var_os("HOME")
        .filter(|path| !path.to_string_lossy().is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn denies(command: &str) -> bool {
        evaluate_command(command, None, false).reason.is_some()
    }

    fn allows(command: &str) -> bool {
        !denies(command)
    }

    #[test]
    fn sentinel_phrase_denies() {
        let command = concat!("echo ", "bad", " cmd");
        let reason = evaluate_command(command, None, false).reason.unwrap();
        assert!(reason.contains(SENTINEL_PHRASE));
    }

    #[test]
    fn blocks_bare_rust_tools() {
        for tool in RUST_TOOLS {
            assert!(
                denies(&format!("{tool} --version")),
                "{tool} should be denied"
            );
            assert!(
                denies(&format!("C:/tools/{tool}.exe --version")),
                "{tool}.exe should be denied"
            );
            assert!(
                denies(&format!(r"C:\tools\{tool}.cmd --version")),
                "{tool}.cmd should be denied"
            );
        }
    }

    #[test]
    fn allows_soldr_prefixed_rust_tools() {
        assert!(allows(&format!("soldr {TOOL_RS_BUILD} build")));
        assert!(allows(&format!(
            "echo before && soldr {TOOL_RS_COMPILER} --version"
        )));
    }

    #[test]
    fn env_prefixed_rust_tools_are_denied() {
        assert!(denies(&format!("FOO=bar {TOOL_RS_BUILD} build")));
        assert!(denies(&format!("env FOO=bar {TOOL_RS_BUILD} build")));
    }

    #[test]
    fn legacy_trampolines_are_denied() {
        for tool in LEGACY_RUST_TRAMPOLINES {
            assert!(denies(&format!("{tool} build")), "{tool} should be denied");
            assert!(
                denies(&format!("uv run {tool} build")),
                "uv run {tool} should be denied"
            );
        }
    }

    #[test]
    fn uv_run_rust_tools_are_denied() {
        assert!(denies(&format!("uv run {TOOL_RS_BUILD} test")));
        assert!(denies(&format!("uv run --with foo {TOOL_RS_BUILD} test")));
        assert!(denies(&format!("uv run --no-sync {TOOL_RS_BUILD} test")));
        assert!(denies(&format!("uv run --no-project {TOOL_RS_BUILD} test")));
        assert!(denies(&format!(
            "uv run --frozen {TOOL_RS_COMPILER} --version"
        )));
        assert!(denies(&format!("uv run --no-binary {TOOL_RS_BUILD} test")));
        assert!(denies(&format!(
            "uv run --with=foo {TOOL_RS_COMPILER} --version"
        )));
        assert!(allows(&format!("uv run --with {TOOL_RS_BUILD} python -V")));
        assert!(allows(&format!("uv run -w {TOOL_RS_BUILD} python -V")));
        assert!(allows(&format!("uv run -m {TOOL_RS_BUILD}")));
        assert!(allows("uv run --script some.py"));
        assert!(allows("uv run --script=some.py"));
    }

    #[test]
    fn nested_shell_wrappers_are_denied() {
        for command in [
            format!("cmd /c {TOOL_RS_BUILD} build"),
            format!("powershell -Command {TOOL_RS_BUILD} build"),
            format!("pwsh -c {TOOL_RS_BUILD} build"),
            format!("bash -c '{TOOL_RS_BUILD} build'"),
            format!("sh -c '{TOOL_RS_BUILD} build'"),
        ] {
            assert!(denies(&command), "{command} should be denied");
        }
    }

    #[test]
    fn quoted_mentions_are_not_invocations() {
        assert!(allows(&format!("echo '{TOOL_RS_BUILD} build'")));
        assert!(allows(&format!("printf \"{TOOL_RS_COMPILER}\"")));
    }

    #[test]
    fn shell_segments_are_scanned_independently() {
        assert!(denies(&format!("echo ok; {TOOL_RS_BUILD} build")));
        assert!(denies(&format!("echo ok && {TOOL_RS_COMPILER} --version")));
        assert!(denies(&format!("echo ok || {TOOL_RS_FORMAT} --version")));
        assert!(allows(&format!("echo 'ok && {TOOL_RS_BUILD} build'")));
    }

    #[test]
    fn hybrid_uv_run_blocks_only_polyglot_roots_without_safe_flags() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("pyproject.toml"), "[project]\nname='x'\n").unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        let nested = root.join("a/b");
        std::fs::create_dir_all(&nested).unwrap();

        assert!(evaluate_command("uv run python -V", Some(&nested), false)
            .reason
            .is_some());
        assert!(
            evaluate_command("uv run --no-sync python -V", Some(&nested), false)
                .reason
                .is_none()
        );
        assert!(
            evaluate_command("uv run --no-project python -V", Some(&nested), false)
                .reason
                .is_none()
        );
        assert!(
            evaluate_command("uv run --frozen python -V", Some(&nested), false)
                .reason
                .is_none()
        );
    }

    #[test]
    fn hybrid_uv_run_allow_all_bypasses_only_hybrid_auto_sync_case() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("pyproject.toml"), "[project]\nname='x'\n").unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();

        let allowed = evaluate_command("uv run python -V", Some(root), true);
        assert!(allowed.reason.is_none());
        assert_eq!(allowed.warnings.len(), 1);
        assert!(
            evaluate_command(&format!("uv run {TOOL_RS_BUILD} test"), Some(root), true)
                .reason
                .is_some(),
            "bypass must not allow direct Rust tool execution"
        );
    }

    #[test]
    fn pure_python_or_pure_rust_roots_do_not_trigger_hybrid_block() {
        let py = tempdir().unwrap();
        std::fs::write(py.path().join("pyproject.toml"), "[project]\nname='x'\n").unwrap();
        assert!(evaluate_command("uv run python -V", Some(py.path()), false)
            .reason
            .is_none());

        let rs = tempdir().unwrap();
        std::fs::write(rs.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        assert!(evaluate_command("uv run python -V", Some(rs.path()), false)
            .reason
            .is_none());
    }

    #[test]
    fn payload_aliases_are_supported() {
        let cwd = PathBuf::from("repo");
        let payload = format!(
            r#"{{"toolName":"Shell","toolInput":{{"argv":["{}","test"]}},"cwdPath":"repo"}}"#,
            TOOL_RS_BUILD
        );
        let parsed = parse_payload(&payload, Path::new(".")).unwrap();
        assert_eq!(parsed.tool_name, "Shell");
        assert_eq!(parsed.command, format!("{TOOL_RS_BUILD} test"));
        assert_eq!(parsed.cwd, cwd);
        assert!(matches!(
            decision_from_payload(&parsed),
            Decision::Deny { .. }
        ));
    }

    #[test]
    fn deny_json_matches_hook_contract() {
        let value = deny_json("nope");
        assert_eq!(
            value["hookSpecificOutput"]["hookEventName"],
            Value::String("PreToolUse".to_string())
        );
        assert_eq!(
            value["hookSpecificOutput"]["permissionDecision"],
            Value::String("deny".to_string())
        );
        assert_eq!(
            value["hookSpecificOutput"]["permissionDecisionReason"],
            Value::String("nope".to_string())
        );
    }
}
