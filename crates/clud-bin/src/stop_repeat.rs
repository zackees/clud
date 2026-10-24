//! Repeated Stop-hook continuations (#1597).
//!
//! A `/goal` condition the sandbox cannot satisfy keeps the harness's Stop
//! evaluator re-prompting, and the agent answers each round with the same
//! message. The loop belongs to the harness (its Stop hook is prompt-based
//! and its precedence over other Stop hooks is undocumented), so clud only
//! detects it: when the same final message ends [`REPEATS`] consecutive
//! hook-driven continuations, the user is told once how to break it. clud
//! never blocks or ends the turn here.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Identical consecutive continuations before the user is told.
pub const REPEATS: u32 = 2;

/// What the user sees once a repeat is detected.
pub const MESSAGE: &str = "clud: the agent has given the same answer to consecutive Stop-hook \
     continuations. If a /goal condition cannot be satisfied in this sandbox (a refused path or \
     command), resolve that step yourself, relax the condition, or run /goal clear.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct State {
    pub hash: u64,
    pub count: u32,
}

/// The next state for one Stop event, and whether to tell the user now.
/// `message` is `None` when the stop was not hook-driven: that resets.
#[must_use]
pub fn step(previous: State, message: Option<&str>) -> (State, bool) {
    let Some(text) = message.map(str::trim).filter(|t| !t.is_empty()) else {
        return (State::default(), false);
    };
    let hash = fnv1a(text);
    let count = if previous.count > 0 && previous.hash == hash {
        previous.count + 1
    } else {
        1
    };
    (State { hash, count }, count == REPEATS)
}

fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Handle one Stop payload; prints a `systemMessage` the first time a repeat
/// is seen. Best effort: any failure is silent.
/// `SessionEnd` drops the session's state file.
pub fn observe(event: &str, stdin: &str) {
    if event != "Stop" && event != "SessionEnd" {
        return;
    }
    let Ok(payload) = serde_json::from_str::<Value>(stdin) else {
        return;
    };
    let Ok(state_dir) = crate::daemon::default_state_dir() else {
        return;
    };
    if event == "SessionEnd" {
        if let Some(session) = session_of(&payload) {
            let _ = std::fs::remove_file(state_path(&state_dir, session));
        }
        return;
    }
    if let Some(line) = observe_in(&state_dir, &payload) {
        println!("{line}");
    }
}

/// The payload's session id, when it is safe to use as a file name.
fn session_of(payload: &Value) -> Option<&str> {
    let session = payload.get("session_id")?.as_str()?;
    let safe = !session.is_empty()
        && session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    safe.then_some(session)
}

/// [`observe`] against `state_dir`; returns the line to print, if any.
pub fn observe_in(state_dir: &Path, payload: &Value) -> Option<String> {
    let session = session_of(payload)?;
    let message = (payload.get("stop_hook_active").and_then(Value::as_bool) == Some(true))
        .then(|| {
            payload
                .get("last_assistant_message")
                .and_then(Value::as_str)
        })
        .flatten();
    let path = state_path(state_dir, session);
    let (next, tell) = step(load(&path), message);
    store(&path, next);
    tell.then(|| serde_json::json!({ "systemMessage": MESSAGE }).to_string())
}

fn state_path(state_dir: &Path, session: &str) -> PathBuf {
    state_dir
        .join("stop-repeat")
        .join(format!("{session}.json"))
}

fn load(path: &Path) -> State {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .map(|v| State {
            hash: v["hash"].as_u64().unwrap_or(0),
            count: v["count"]
                .as_u64()
                .and_then(|c| u32::try_from(c).ok())
                .unwrap_or(0),
        })
        .unwrap_or_default()
}

fn store(path: &Path, state: State) {
    if state == State::default() {
        let _ = std::fs::remove_file(path);
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = serde_json::json!({ "hash": state.hash, "count": state.count });
    let _ = std::fs::write(path, body.to_string());
}

#[cfg(test)]
#[path = "stop_repeat_tests.rs"]
mod tests;
