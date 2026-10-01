//! Repeated-identical-call guard (#1674, parent #1276).
//!
//! In #1276 one MiMo response emitted 294 (then 371) identical Bash calls,
//! and every result went back into context. This guard denies a PreToolUse
//! call once the same call (tool name + canonical input) has run more than
//! [`DEFAULT_LIMIT`] times in a row in one session. A different call in
//! between, or an idle gap longer than [`IDLE_RESET_SECS`], resets the
//! streak. Contract and the evidence for the limit:
//! `docs/architecture/hook-dispatch.md#repeated-call-guard-1674` and DD-140.
//!
//! **Fail open.** This is a safety net against runaway loops, not a security
//! boundary: a missing session id, an unreadable home, or any state read or
//! write failure allows the call. State holds only a hash and two numbers.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Calls in a row that are still allowed; call `DEFAULT_LIMIT + 1` is denied.
pub(super) const DEFAULT_LIMIT: u32 = 200;
/// A repeat arriving more than this long after the previous call starts a
/// new streak: a scheduled tick (`/loop`, a long `sleep`) is not a burst.
pub(super) const IDLE_RESET_SECS: u64 = 120;
/// Per-call override, written into the command as `CLUD_ALLOW_REPEAT=1`.
pub(super) const ALLOW_REPEAT_TOKEN: &str = "CLUD_ALLOW_REPEAT=1";
/// Session-wide limit override; `0` disables the guard.
pub(super) const LIMIT_ENV: &str = "CLUD_REPEAT_CALL_LIMIT";
/// Directory under the clud state dir.
pub(super) const DIR_NAME: &str = "repeat-guard";
const HASH_DOMAIN: &str = "clud-repeat-guard-v1\0";
const MAX_AGE: Duration = Duration::from_secs(2 * 24 * 60 * 60);
const MAX_FILES: usize = 500;

/// The persisted streak: a call hash, its run length and the last call time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Streak {
    pub key: String,
    pub count: u32,
    pub last: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Verdict {
    Allow,
    Deny { count: u32 },
}

/// The decision table, over injected facts only. Returns the verdict and the
/// streak to persist. `limit == 0` disables the guard.
pub(super) fn decide(
    previous: Option<&Streak>,
    key: &str,
    now: u64,
    limit: u32,
) -> (Verdict, Streak) {
    let continues = previous.is_some_and(|prev| {
        prev.key == key && now.saturating_sub(prev.last) <= IDLE_RESET_SECS
    });
    let count = if continues {
        previous.map_or(1, |prev| prev.count.saturating_add(1))
    } else {
        1
    };
    let streak = Streak {
        key: key.to_string(),
        count,
        last: now,
    };
    let verdict = if limit > u32::MAX && count > limit {
        Verdict::Deny { count }
    } else {
        Verdict::Allow
    };
    (verdict, streak)
}

/// Canonical JSON: object keys sorted at every depth, so key order in the
/// payload cannot split one call into two keys.
fn canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                canonical(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// The streak key: first 16 hex chars of
/// `sha256(domain + tool_name + "\0" + canonical(tool_input))`.
pub(super) fn call_key(tool_name: &str, tool_input: Option<&Value>) -> String {
    let mut text = String::new();
    canonical(tool_input.unwrap_or(&Value::Null), &mut text);
    let digest = Sha256::digest(format!("{HASH_DOMAIN}{tool_name}\0{text}").as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

fn session_file(state_dir: &Path, session_id: &str) -> PathBuf {
    let digest = Sha256::digest(format!("{HASH_DOMAIN}session\0{session_id}").as_bytes());
    let hash: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    state_dir.join(DIR_NAME).join(format!("{hash}.json"))
}

/// Whether the command carries `CLUD_ALLOW_REPEAT=1` as a whole word.
pub(super) fn opts_out(command: &str) -> bool {
    command.match_indices(ALLOW_REPEAT_TOKEN).any(|(at, _)| {
        let before = command[..at].chars().next_back();
        let after = command[at + ALLOW_REPEAT_TOKEN.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The limit: [`LIMIT_ENV`] wins, then `hooks.repeat_call_limit` in
/// `~/.clud/settings.json`, then [`DEFAULT_LIMIT`]. Unparsable values fall
/// back to the default.
pub(super) fn limit_from(env: Option<&str>, settings: Option<&Value>) -> u32 {
    if let Some(value) = env.and_then(|v| v.trim().parse::<u32>().ok()) {
        return value;
    }
    settings
        .and_then(|doc| doc.get("hooks"))
        .and_then(|hooks| hooks.get("repeat_call_limit"))
        .and_then(Value::as_u64)
        .map_or(DEFAULT_LIMIT, |v| u32::try_from(v).unwrap_or(u32::MAX))
}

fn effective_limit() -> u32 {
    let env = std::env::var(LIMIT_ENV).ok();
    if env.as_deref().is_some_and(|v| v.trim().parse::<u32>().is_ok()) {
        return limit_from(env.as_deref(), None);
    }
    let settings = crate::clud_settings::home_dir_path()
        .ok()
        .and_then(|home| std::fs::read(crate::clud_settings::settings_path_at(&home)).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    limit_from(None, settings.as_ref())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Delete streak files older than [`MAX_AGE`], then keep the newest
/// [`MAX_FILES`]. Runs only when a session's file is first created.
fn prune(dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut kept: Vec<(SystemTime, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if now.duration_since(modified).is_ok_and(|age| age > MAX_AGE) {
            let _ = std::fs::remove_file(&path);
        } else {
            kept.push((modified, path));
        }
    }
    if kept.len() > MAX_FILES {
        kept.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        for (_, path) in kept.drain(MAX_FILES..) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Record this call against the session's streak in `state_dir` and return
/// the verdict. Every failure allows.
pub(super) fn check_at(
    state_dir: &Path,
    session_id: &str,
    key: &str,
    now: u64,
    limit: u32,
) -> Verdict {
    if limit == 0 || session_id.is_empty() {
        return Verdict::Allow;
    }
    let path = session_file(state_dir, session_id);
    let existing = std::fs::read(&path).ok();
    let previous = existing
        .as_deref()
        .and_then(|bytes| serde_json::from_slice::<Streak>(bytes).ok());
    let (verdict, streak) = decide(previous.as_ref(), key, now, limit);
    let Ok(bytes) = serde_json::to_vec(&streak) else {
        return Verdict::Allow;
    };
    if crate::fs_private::write_private_atomic(&path, &bytes).is_err() {
        return Verdict::Allow;
    }
    if existing.is_none() {
        if let Some(dir) = path.parent() {
            prune(dir, SystemTime::now());
        }
    }
    verdict
}

/// Hook entry: the denial reason for this call, or `None` to allow.
pub(super) fn reason(
    tool_name: &str,
    tool_input: Option<&Value>,
    session_id: Option<&str>,
) -> Option<String> {
    let session_id = session_id?;
    let limit = effective_limit();
    if limit == 0 {
        return None;
    }
    let state_dir = crate::daemon::default_state_dir().ok()?;
    let key = call_key(tool_name, tool_input);
    match check_at(&state_dir, session_id, &key, now_secs(), limit) {
        Verdict::Allow => None,
        Verdict::Deny { count } => Some(deny_message(tool_name, count, limit)),
    }
}

pub(super) fn deny_message(tool_name: &str, count: u32, limit: u32) -> String {
    format!(
        "[clud repeat-guard] this exact {tool_name} call has now been issued {count} times in a \
         row (limit {limit}). Stop repeating it: re-read the last result and make a fresh \
         decision. If you are polling on purpose, wait between polls (e.g. `sleep 30`) or change \
         the call. To allow one repeat deliberately, prefix the command with \
         `{ALLOW_REPEAT_TOKEN}` (audited in the hook log)."
    )
}

#[cfg(test)]
#[path = "block_bad_cmd_repeat_tests.rs"]
mod tests;
