//! Issue #260: agent-memory hook subcommands.
//!
//! Claude Code and Codex invoke these subcommands at session lifecycle
//! events. Each handler:
//!
//! - Reads a JSON payload from stdin (capped at 1 MiB).
//! - Talks to the daemon's `/memory/*` HTTP routes via
//!   `daemon::memory_client` (single SQLite writer per process —
//!   [DD-019]).
//! - Always exits 0. A hook failure must never block the agent. Errors
//!   are surfaced on stderr only when `CLUD_MEMORY_DEBUG_HOOKS=1`.
//!
//! Hook registration to `~/.claude/settings.json` /
//! `~/.codex/hooks.json` is owned by sibling #265; this module owns the
//! subcommand handlers only.
//!
//! Stdin contract (the schemas live with the handlers below):
//!
//! | Verb               | Trigger                                  |
//! |--------------------|------------------------------------------|
//! | `session-start`    | Claude/Codex session opens               |
//! | `user-prompt-submit` | User submits a prompt                  |
//! | `post-tool-use`    | After a tool call                        |
//! | `stop`             | Session ends                             |
//!
//! Stdout is reserved for `session-start` (a `<context>` block injected
//! into the agent's system prompt). The other three hooks write nothing
//! on success — Claude treats hook stdout as additional context for the
//! current turn, so chatty hooks pollute the agent transcript.

use std::io::{self, Read};
use std::path::Path;

use serde::Deserialize;

use crate::args::HookSubcommand;
use crate::daemon::{self, MemoryHttpResponse};

/// Hard cap on a stdin payload. The four hook events ship single-page
/// JSON; 1 MiB is generous.
const MAX_STDIN_BYTES: usize = 1024 * 1024;

/// Env-var that turns on verbose stderr logging from the hook handlers.
/// Off by default — agent transcripts must stay clean.
const ENV_DEBUG: &str = "CLUD_MEMORY_DEBUG_HOOKS";

/// Env-var that opts into consolidation on the `Stop` hook. Default is
/// off because the daemon already runs a consolidation timer; the
/// Stop-hook path is for users who want one final tick before close.
const ENV_AUTO_CONSOLIDATE_ON_STOP: &str = "CLUD_MEMORY_AUTO_CONSOLIDATE_ON_STOP";

/// Number of recent-memory rows to include in the SessionStart context
/// block. Capped to keep the daemon-side latency bounded.
const SESSION_START_K: u32 = 20;

/// Public entry point dispatched from `main.rs`.
///
/// Returns the process exit code. All paths return `0`; the explicit
/// return is the documented contract surface for callers.
pub fn dispatch(sub: HookSubcommand) -> i32 {
    match sub {
        HookSubcommand::SessionStart => run_session_start(io::stdin(), io::stdout()),
        HookSubcommand::UserPromptSubmit => run_user_prompt_submit(io::stdin()),
        HookSubcommand::PostToolUse => run_post_tool_use(io::stdin()),
        HookSubcommand::Stop => run_stop(io::stdin()),
    }
}

/// Pre-parse peek used by `main.rs` to dispatch a hook subcommand
/// BEFORE clap runs. The full clap parse triggers `--version` /
/// `--help` exits that would skip downstream side-effects (notably
/// `console_title::set_for_current_cwd()`), so for the hook path we
/// scan the argv directly for the `hook <subcommand>` pair.
///
/// Returns `None` when no recognized hook subcommand is present —
/// the rest of `main` falls through to the normal clap parse.
pub fn peek_hook_subcommand_from_argv(
    argv: impl IntoIterator<Item = String>,
) -> Option<HookSubcommand> {
    let mut iter = argv.into_iter();
    // argv[0] is the program path; skip it.
    let _ = iter.next();
    let mut saw_hook = false;
    for arg in iter {
        if arg == "--" {
            // Everything after `--` is backend passthrough — never a
            // hook subcommand.
            return None;
        }
        if !saw_hook {
            if arg == "hook" {
                saw_hook = true;
            }
            continue;
        }
        return match arg.as_str() {
            "session-start" => Some(HookSubcommand::SessionStart),
            "user-prompt-submit" => Some(HookSubcommand::UserPromptSubmit),
            "post-tool-use" => Some(HookSubcommand::PostToolUse),
            "stop" => Some(HookSubcommand::Stop),
            // Unknown verb (`clud hook --help`, `clud hook foo`, …):
            // fall through and let clap report the error.
            _ => None,
        };
    }
    None
}

// ---------- payload shapes ----------

/// Claude Code `SessionStart` / Codex `session_start` payload.
///
/// `#[serde(default)]` on every field so a partial payload still
/// deserializes — the daemon-down + bad-stdin paths still need to exit
/// 0.
///
/// `cwd`, `model`, `source`, and `transcript_path` are recorded for
/// future-use (auto-export, transcript-based recall) but the v0.1
/// SessionStart handler only routes off `session_id`.
#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)]
struct SessionStartPayload {
    #[serde(default, alias = "session-id")]
    session_id: String,
    #[serde(default, alias = "working_directory", alias = "working-directory")]
    cwd: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default, alias = "transcript-path")]
    transcript_path: Option<String>,
}

/// Claude Code `UserPromptSubmit` / Codex `user_prompt_submit` payload.
///
/// `working_directory` is recorded for future scope-key resolution but
/// the v0.1 handler routes only off `session_id` and the prompt text.
#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)]
struct UserPromptSubmitPayload {
    #[serde(default, alias = "session-id")]
    session_id: String,
    #[serde(default)]
    prompt: String,
    #[serde(default, alias = "cwd")]
    working_directory: Option<String>,
}

/// Claude Code `PostToolUse` / Codex `post_tool_use` payload.
///
/// Codex emits `tool_call` / `tool_result`; aliases bridge both shapes.
#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)] // v0.1 is a logged no-op; fields are documented for v0.5.
struct PostToolUsePayload {
    #[serde(default, alias = "session-id")]
    session_id: String,
    #[serde(default)]
    tool_name: String,
    #[serde(default, alias = "tool_call")]
    tool_input: serde_json::Value,
    #[serde(default, alias = "tool_result", alias = "tool_output")]
    tool_response: serde_json::Value,
    #[serde(default, alias = "cwd")]
    working_directory: Option<String>,
}

/// Claude Code `Stop` / Codex `session_end` payload.
///
/// `stop_hook_active` and `working_directory` are recorded for future
/// re-entrancy and scope-key resolution but the v0.1 handler routes
/// only off `session_id` and `reason`.
#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)]
struct StopPayload {
    #[serde(default, alias = "session-id")]
    session_id: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    stop_hook_active: bool,
    #[serde(default, alias = "cwd")]
    working_directory: Option<String>,
}

// ---------- runners ----------

fn run_session_start(reader: impl Read, mut writer: impl io::Write) -> i32 {
    let raw = read_stdin_capped(reader);
    let payload = match parse_json::<SessionStartPayload>(&raw) {
        Ok(p) => p,
        Err(e) => {
            debug_log(format_args!("session-start: bad payload: {e}"));
            // Even on bad input, emit an empty context block so the
            // upstream injection point doesn't have to special-case a
            // missing block.
            let _ = writer.write_all(empty_context_block().as_bytes());
            return 0;
        }
    };

    let recall = match recall_memories(payload.session_id.as_str(), SESSION_START_K) {
        Ok(rows) => rows,
        Err(e) => {
            debug_log(format_args!("session-start: recall failed: {e}"));
            let _ = writer.write_all(empty_context_block().as_bytes());
            return 0;
        }
    };

    let block = format_context_block(&recall);
    let _ = writer.write_all(block.as_bytes());
    0
}

fn run_user_prompt_submit(reader: impl Read) -> i32 {
    let raw = read_stdin_capped(reader);
    let payload = match parse_json::<UserPromptSubmitPayload>(&raw) {
        Ok(p) => p,
        Err(e) => {
            debug_log(format_args!("user-prompt-submit: bad payload: {e}"));
            return 0;
        }
    };

    let Some(directive) = extract_save_directive(&payload.prompt) else {
        // Conservative default: do not auto-save every prompt. The user
        // opts in via the explicit `clud memory save` verb or via
        // `remember:` / `save this:` directives in the prompt itself.
        return 0;
    };
    if directive.is_empty() {
        return 0;
    }

    let session_id = if payload.session_id.is_empty() {
        None
    } else {
        Some(payload.session_id.as_str())
    };
    if let Err(e) = save_memory(&directive, "working", session_id) {
        debug_log(format_args!("user-prompt-submit: save failed: {e}"));
    }
    0
}

fn run_post_tool_use(reader: impl Read) -> i32 {
    let raw = read_stdin_capped(reader);
    match parse_json::<PostToolUsePayload>(&raw) {
        Ok(p) => {
            // v0.1 ships as a logged no-op. Tool-output classification
            // (which results become Working-tier "lesson" rows) lands
            // alongside the daemon's `/memory/working/append` route in
            // v0.5.
            debug_log(format_args!(
                "post-tool-use: session={} tool={} (no-op v0.1)",
                p.session_id, p.tool_name,
            ));
        }
        Err(e) => {
            debug_log(format_args!("post-tool-use: bad payload: {e}"));
        }
    }
    0
}

fn run_stop(reader: impl Read) -> i32 {
    let raw = read_stdin_capped(reader);
    let payload = match parse_json::<StopPayload>(&raw) {
        Ok(p) => p,
        Err(e) => {
            debug_log(format_args!("stop: bad payload: {e}"));
            return 0;
        }
    };

    if !auto_consolidate_on_stop_enabled() {
        debug_log(format_args!(
            "stop: session={} reason={:?} (consolidate disabled)",
            payload.session_id, payload.reason,
        ));
        return 0;
    }

    // `/memory/consolidate` does not exist yet — the consolidation
    // timer inside the daemon owns the schedule today (see
    // docs/architecture/memory.md). When the route lands (planned for
    // #258 follow-up), this path will POST to it. For now we log and
    // fall through so the user sees a clear message in debug mode.
    debug_log(format_args!(
        "stop: session={} consolidate route not yet wired (TODO)",
        payload.session_id,
    ));
    0
}

// ---------- helpers ----------

/// Read up to `MAX_STDIN_BYTES` from `reader`. Trailing bytes are
/// silently dropped — a misbehaving caller can't OOM the hook. UTF-8
/// is decoded lossily so a Windows-1252 console handle doesn't trip up
/// the parse.
fn read_stdin_capped(mut reader: impl Read) -> String {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let take = n.min(MAX_STDIN_BYTES.saturating_sub(buf.len()));
                buf.extend_from_slice(&chunk[..take]);
                if buf.len() >= MAX_STDIN_BYTES {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn parse_json<T: for<'de> Deserialize<'de>>(raw: &str) -> Result<T, serde_json::Error> {
    let trimmed = raw.trim();
    // An empty payload still deserializes to `Default` because every
    // field is `#[serde(default)]`.
    let source = if trimmed.is_empty() { "{}" } else { trimmed };
    serde_json::from_str(source)
}

fn debug_log(args: std::fmt::Arguments<'_>) {
    if std::env::var_os(ENV_DEBUG).is_some() {
        eprintln!("[clud-hook] {args}");
    }
}

fn auto_consolidate_on_stop_enabled() -> bool {
    matches!(
        std::env::var(ENV_AUTO_CONSOLIDATE_ON_STOP).ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Recognize an opt-in save directive in the user's prompt. Case-
/// insensitive; trims the leading directive and returns the body.
///
/// Returns `None` when the prompt does not begin with one of the
/// recognized directives. The default (no directive) is to do nothing,
/// keeping the hook from silently logging every prompt.
fn extract_save_directive(prompt: &str) -> Option<String> {
    let trimmed = prompt.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    let directives = [
        "remember:",
        "remember this:",
        "save this:",
        "save:",
        "memorize:",
    ];
    for d in directives {
        if let Some(rest) = lower.strip_prefix(d) {
            let start = trimmed.len() - rest.len();
            return Some(trimmed[start..].trim().to_string());
        }
    }
    None
}

fn empty_context_block() -> String {
    String::from("<context source=\"clud-memory\">\n## Recent memory\n(none)\n</context>\n")
}

/// Format a `<context>` block of recalled memory rows for stdout.
///
/// Claude Code injects this block into the system prompt; Codex
/// surfaces it as a visible system message. The format is stable: any
/// downstream consumer that parses the block keys off the
/// `source="clud-memory"` attribute and the `[tier]` bullets.
fn format_context_block(rows: &[RecallRow]) -> String {
    if rows.is_empty() {
        return empty_context_block();
    }
    let mut out = String::with_capacity(64 + rows.len() * 80);
    out.push_str("<context source=\"clud-memory\">\n");
    out.push_str("## Recent memory\n");
    for row in rows {
        let preview = excerpt(&row.content, 160);
        out.push_str(&format!("- [{}] {}\n", row.tier, preview));
    }
    out.push_str("</context>\n");
    out
}

fn excerpt(s: &str, max: usize) -> String {
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        collapsed
    } else {
        let truncated: String = collapsed.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

/// Lightweight projection used by the context-block formatter. Keeps
/// the runners decoupled from the wire shape so unit tests can drive
/// the formatter with fixtures.
#[derive(Debug, Clone)]
struct RecallRow {
    tier: String,
    content: String,
}

/// Recall up to `k` memories for `session_id` via the daemon's
/// `/memory/search` route. Empty `session_id` falls back to a global
/// query; the search route treats `session_id = None` as
/// "do not filter by session".
fn recall_memories(session_id: &str, k: u32) -> io::Result<Vec<RecallRow>> {
    let state_dir =
        daemon::default_state_dir().map_err(|e| io::Error::other(format!("state dir: {e}")))?;
    fetch_recall(&state_dir, session_id, k)
}

fn fetch_recall(state_dir: &Path, session_id: &str, k: u32) -> io::Result<Vec<RecallRow>> {
    // `/memory/search` requires a non-empty `q`. For the SessionStart
    // path we want "any recent memory for this session", so we hit
    // `/memory/recent` instead — it returns the newest rows directly
    // and does not depend on the embedder.
    //
    // We narrow client-side by session_id since `/memory/recent` does
    // not currently filter by session — when 0 hits remain, fall back
    // to the un-narrowed list (still capped at k).
    let resp: MemoryHttpResponse = daemon::http_recent(state_dir, k as usize * 4)?;
    if resp.status >= 500 {
        return Err(io::Error::other(format!(
            "memory recent {} {}",
            resp.status, resp.body
        )));
    }
    if resp.status != 200 {
        return Ok(Vec::new());
    }
    let parsed: serde_json::Value =
        serde_json::from_str(&resp.body).unwrap_or(serde_json::Value::Null);
    let empty = Vec::new();
    let rows = parsed.as_array().unwrap_or(&empty);
    let mut out = Vec::with_capacity(k as usize);
    let filter_session = !session_id.is_empty();
    for row in rows {
        let row_session = row.get("session_id").and_then(|v| v.as_str());
        if filter_session && row_session != Some(session_id) {
            continue;
        }
        let tier = row
            .get("tier")
            .and_then(|v| v.as_str())
            .unwrap_or("working")
            .to_string();
        let content = row
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if content.is_empty() {
            continue;
        }
        out.push(RecallRow { tier, content });
        if out.len() >= k as usize {
            break;
        }
    }
    // If session filtering left us empty (eg first turn of a new
    // session), surface the global newest rows as a fallback — first-
    // turn injections are most useful with at least some recall.
    if out.is_empty() && filter_session {
        for row in rows {
            let tier = row
                .get("tier")
                .and_then(|v| v.as_str())
                .unwrap_or("working")
                .to_string();
            let content = row
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if content.is_empty() {
                continue;
            }
            out.push(RecallRow { tier, content });
            if out.len() >= k as usize {
                break;
            }
        }
    }
    Ok(out)
}

/// `POST /memory/save` wrapper used by the `user-prompt-submit`
/// directive path.
fn save_memory(content: &str, tier: &str, session_id: Option<&str>) -> io::Result<()> {
    let state_dir =
        daemon::default_state_dir().map_err(|e| io::Error::other(format!("state dir: {e}")))?;
    let payload = serde_json::json!({
        "content": content,
        "tier": tier,
        "session_id": session_id,
    })
    .to_string();
    let resp: MemoryHttpResponse = daemon::http_save(&state_dir, &payload)?;
    if resp.status != 200 {
        return Err(io::Error::other(format!(
            "memory save {} {}",
            resp.status, resp.body
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
