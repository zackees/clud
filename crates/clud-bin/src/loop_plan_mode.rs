//! Session-scoped automatic plan-mode guard for native `/loop` (#1844).
//!
//! Scheduling events are applied under the daemon's session-flag mutex, so
//! parallel CronCreate/CronDelete hooks cannot lose another job's state.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ALLOW_ENV: &str = "CLUD_ALLOW_PLAN_MODE";
pub const FLAG_KEY: &str = "loop_plan_mode";
pub const POST_TOOL_USE: &str = "PostToolUse";
pub const DENIAL: &str = "[loop-plan-mode] Automatic plan mode is disabled while /loop or scheduled work is active. Implement the authorized task directly; do not wait for plan approval. --allow-plan-mode explicitly restores it.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    Prompt,
    Created(String),
    Deleted(String),
    Wakeup(bool),
    Inventory(BTreeSet<String>),
    End,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    pending: bool,
    dynamic: bool,
    jobs: BTreeSet<String>,
}

/// Called only while holding the registry mutex.
#[must_use]
pub fn transition(value: Option<Value>, event: Event) -> Option<Value> {
    if matches!(event, Event::End) {
        return None;
    }
    let mut state: State = value
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    match event {
        Event::Prompt => state.pending = true,
        Event::Created(id) => {
            state.jobs.insert(id);
            state.pending = false;
        }
        Event::Deleted(id) => {
            state.jobs.remove(&id);
            state.pending = false;
        }
        Event::Wakeup(active) => {
            state.dynamic = active;
            state.pending = false;
        }
        Event::Inventory(jobs) => {
            state.jobs = jobs;
            state.pending = false;
            state.dynamic = false;
        }
        Event::End => unreachable!(),
    }
    Some(serde_json::to_value(state).expect("loop state is JSON-compatible"))
}

#[must_use]
pub fn active(value: Option<Value>) -> bool {
    value
        .and_then(|value| serde_json::from_value::<State>(value).ok())
        .is_some_and(|state| state.pending || state.dynamic || !state.jobs.is_empty())
}

fn loop_prompt(prompt: &str) -> bool {
    prompt
        .trim_start()
        .strip_prefix("/loop")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

fn id(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|id| crate::grind_facts::valid_session_id(id))
        .map(str::to_string)
}

/// Interpret successful tool events only; failures never establish activity.
fn tool_event(payload: &Value) -> Option<Event> {
    let response = payload.get("tool_response")?;
    if response.get("is_error").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    match payload.get("tool_name")?.as_str()? {
        "CronCreate" => {
            id(response.get("id").or_else(|| response.get("jobId"))).map(Event::Created)
        }
        "CronDelete" => id(payload
            .pointer("/tool_input/id")
            .or_else(|| payload.pointer("/tool_input/jobId")))
        .map(Event::Deleted),
        "ScheduleWakeup" => Some(Event::Wakeup(
            payload.pointer("/tool_input/stop").and_then(Value::as_bool) != Some(true),
        )),
        _ => None,
    }
}

/// Record native prompt/scheduler events. Never blocks a prompt or tool result.
/// The caller still runs any repository-declared hooks for the same event.
pub fn observe(event: &str, stdin: &str) {
    if !matches!(
        event,
        "UserPromptSubmit" | POST_TOOL_USE | "Stop" | "SessionEnd"
    ) {
        return;
    }
    if crate::runner::unsafe_mode_enabled() || std::env::var(ALLOW_ENV).ok().as_deref() == Some("1")
    {
        return;
    }
    let Ok(payload) = serde_json::from_str::<Value>(stdin) else {
        return;
    };
    let Some(session) = id(payload.get("session_id")) else {
        return;
    };
    let update = observed_event(event, &payload);
    let Ok(state_dir) = crate::daemon::default_state_dir() else {
        return;
    };
    if let Some(update) = update {
        if let Err(error) = crate::daemon::session_loop_event(&state_dir, &session, update) {
            eprintln!("[clud loop] could not record scheduling state: {error}");
        }
    }
    recovery_context(event, &payload, &state_dir, &session);
}

fn observed_event(event: &str, payload: &Value) -> Option<Event> {
    match event {
        "UserPromptSubmit" if loop_prompt(payload["prompt"].as_str().unwrap_or("")) => {
            Some(Event::Prompt)
        }
        POST_TOOL_USE => tool_event(&payload),
        "Stop" => payload
            .get("session_crons")
            .and_then(Value::as_array)
            .map(|jobs| {
                Event::Inventory(jobs.iter().filter_map(|job| id(job.get("id"))).collect())
            }),
        "SessionEnd" => Some(Event::End),
        _ => None,
    }
}

fn recovery_context(event: &str, payload: &Value, state_dir: &Path, session: &str) {
    // A hook cannot change Claude's permission mode. Give a loop already
    // started in manual plan mode an actionable recovery, without blocking it.
    if event == "UserPromptSubmit"
        && payload["permission_mode"] == "plan"
        && refusal_in(state_dir, Some(session), false).is_some()
    {
        println!(
            "{}",
            serde_json::json!({"hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": "Scheduled work must continue without plan approval. This session is already in plan mode: use ExitPlanMode to leave it, or ask the user to switch out with Shift+Tab if approval is required. clud hooks cannot force a permission-mode change."
            }})
        );
    }
}

#[must_use]
pub fn refusal(session: Option<&str>) -> Option<&'static str> {
    let allowed = std::env::var(ALLOW_ENV).ok().as_deref() == Some("1");
    let state_dir = crate::daemon::default_state_dir().ok()?;
    refusal_in(&state_dir, session, allowed)
}

#[must_use]
pub fn refusal_in(state_dir: &Path, session: Option<&str>, allowed: bool) -> Option<&'static str> {
    if allowed {
        return None;
    }
    let session = session.filter(|id| crate::grind_facts::valid_session_id(id))?;
    let value = crate::daemon::session_flag_get(state_dir, session, FLAG_KEY).ok()?;
    active(value).then_some(DENIAL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_state_tracks_each_job_until_the_last_delete() {
        let state = transition(None, Event::Prompt);
        assert!(active(state.clone()));
        let state = transition(state, Event::Created("a".into()));
        let state = transition(state, Event::Created("b".into()));
        let state = transition(state, Event::Deleted("a".into()));
        assert!(active(state.clone()));
        let state = transition(state, Event::Deleted("b".into()));
        assert!(!active(state));
    }

    #[test]
    fn dynamic_stop_and_authoritative_inventory_clear_activity() {
        let state = transition(None, Event::Wakeup(true));
        assert!(active(state.clone()));
        assert!(!active(transition(state, Event::Wakeup(false))));
        let state = transition(None, Event::Created("expired".into()));
        assert!(!active(transition(
            state,
            Event::Inventory(BTreeSet::new())
        )));
    }

    #[test]
    fn prose_and_similarly_named_commands_are_not_loop_invocations() {
        assert!(loop_prompt(" /loop 20m fix it"));
        assert!(loop_prompt("/loop"));
        for prompt in [
            "explain /loop",
            "`/loop fix it`",
            "/loophole",
            "do not run /loop",
        ] {
            assert!(!loop_prompt(prompt));
        }
    }
}
