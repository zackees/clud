//! #1949: a non-Claude model must not end `/loop` before it ever looped.
//!
//! In dynamic mode, `/loop` hands the model `ScheduleWakeup`, and the model
//! decides each round whether to schedule the next one or call
//! `ScheduleWakeup {"stop": true}`. Codex models routed through the Claude
//! harness read a prompt that sounds finished ("choose the next issue") as a
//! one-shot task and stop at the end of round one, so the loop never runs a
//! second time. Claude models do not.
//!
//! The guard refuses that first stop once, on a non-Claude route only, and
//! tells the model to schedule the next round instead. It lets the stop
//! through when:
//!
//! - the session has scheduled a wakeup before (the loop has really looped,
//!   so a stop is a decision about a running loop);
//! - this round already had a stop refused (the model insists, so the user's
//!   stop condition may really be met);
//! - the transcript has no `/loop` command (`ScheduleWakeup` serves some other
//!   flow), or cannot be read (fail open).

use std::path::Path;

use serde_json::Value;

/// The rule id the model sees, and the marker that a stop was refused.
pub const RULE_ID: &str = "loop-first-round-stop";

/// The harness tool that schedules, or ends, a dynamic `/loop`.
pub(super) const SCHEDULE_WAKEUP: &str = "ScheduleWakeup";

/// The marker a `/loop` invocation leaves in the transcript. Each wakeup
/// re-enters the skill with the same marker, so the last one starts the
/// current round.
const LOOP_COMMAND_MARKER: &str = "<command-name>/loop</command-name>";

/// Whether the launch routes the Claude harness to a non-Claude model.
///
/// Reads the route context every clud child carries. A missing or unreadable
/// context means a launch clud did not route, so the guard stays out of it.
#[must_use]
pub fn non_claude_route(route_context: Option<&str>) -> bool {
    route_context
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|route| {
            route
                .get("model_provider")
                .and_then(Value::as_str)
                .map(|provider| !provider.eq_ignore_ascii_case("claude"))
        })
        .unwrap_or(false)
}

/// Why this `ScheduleWakeup` call is refused, if it is.
///
/// `transcript` is called only for a `ScheduleWakeup` stop on a non-Claude
/// route, so every other tool call skips the read.
#[must_use]
pub fn reason(
    tool_name: &str,
    tool_input: Option<&Value>,
    transcript: impl FnOnce() -> Option<String>,
    non_claude_route: bool,
) -> Option<String> {
    if !non_claude_route || !tool_name.eq_ignore_ascii_case(SCHEDULE_WAKEUP) {
        return None;
    }
    let stops = tool_input
        .and_then(|input| input.get("stop"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !stops || !first_round_stop(&transcript()?) {
        return None;
    }
    Some(format!(
        "[{RULE_ID}] /loop is recurring: the user asked for this prompt to run again, \
         and this would end the loop before it ever ran a second time. Finishing this \
         round's work is not a reason to stop. Call {SCHEDULE_WAKEUP} with \
         delaySeconds, the same /loop prompt and a reason, then end the turn. Stop \
         only when the user's own stop condition is met; if it is met now, call \
         {SCHEDULE_WAKEUP} with stop: true again and it will go through."
    ))
}

/// Whether the transcript is in the first round of a `/loop` that has never
/// scheduled a wakeup and has not already had a stop refused this round.
fn first_round_stop(transcript: &str) -> bool {
    let mut in_loop = false;
    let mut ever_scheduled = false;
    let mut refused_this_round = false;
    for entry in transcript
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
    {
        let Some(content) = entry.pointer("/message/content") else {
            continue;
        };
        if mentions(content, LOOP_COMMAND_MARKER) && entry["type"] == "user" {
            in_loop = true;
            refused_this_round = false;
        }
        for block in content.as_array().into_iter().flatten() {
            match block["type"].as_str() {
                Some("tool_use")
                    if block["name"] == SCHEDULE_WAKEUP
                        && block.pointer("/input/delaySeconds").is_some() =>
                {
                    ever_scheduled = true;
                }
                Some("tool_result") if mentions(&block["content"], RULE_ID) => {
                    refused_this_round = true;
                }
                _ => {}
            }
        }
    }
    in_loop && !ever_scheduled && !refused_this_round
}

/// Whether `needle` appears in a message content value: a string, or the
/// text of any block in a list.
fn mentions(content: &Value, needle: &str) -> bool {
    match content {
        Value::String(text) => text.contains(needle),
        Value::Array(blocks) => blocks.iter().any(|block| {
            block["text"].as_str().is_some_and(|t| t.contains(needle))
                || mentions(&block["content"], needle)
        }),
        _ => false,
    }
}

/// Read the transcript a hook payload names. `None` on any failure, which
/// the guard treats as "allow".
#[must_use]
pub fn read_transcript(path: Option<&Path>) -> Option<String> {
    std::fs::read_to_string(path?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const LOOP_ARGS: &str = "<command-message>loop</command-message>\n\
        <command-name>/loop</command-name>\n\
        <command-args>choose the next issue</command-args>";

    fn entry(kind: &str, content: Value) -> String {
        let message = json!({"role": kind, "content": content});
        format!("{}\n", json!({"type": kind, "message": message}))
    }

    fn loop_command() -> String {
        entry("user", json!(LOOP_ARGS))
    }

    fn wakeup(input: &Value) -> String {
        let block = json!({
            "type": "tool_use", "id": "t1", "name": "ScheduleWakeup", "input": input
        });
        entry("assistant", json!([block]))
    }

    fn result(text: &str) -> String {
        let block = json!({"type": "tool_result", "tool_use_id": "t1", "content": text});
        entry("user", json!([block]))
    }

    fn stop() -> Value {
        json!({"stop": true})
    }

    /// The decision for a call on a route, against an in-memory transcript.
    fn decide(tool: &str, input: &Value, transcript: &str, codex: bool) -> Option<String> {
        reason(tool, Some(input), || Some(transcript.to_string()), codex)
    }

    #[test]
    fn a_first_round_stop_on_a_codex_route_is_refused() {
        let reason = decide("ScheduleWakeup", &stop(), &loop_command(), true)
            .expect("first-round stop must be refused");
        assert!(reason.contains(RULE_ID), "{reason}");
        assert!(reason.contains("delaySeconds"), "{reason}");
    }

    #[test]
    fn a_stop_after_the_loop_has_scheduled_goes_through() {
        let scheduled = json!({"delaySeconds": 1200, "prompt": "/loop x", "reason": "r"});
        let transcript = loop_command()
            + &wakeup(&scheduled)
            + &result("Next wakeup scheduled for 10:00")
            + &loop_command();
        assert_eq!(decide("ScheduleWakeup", &stop(), &transcript, true), None);
    }

    #[test]
    fn a_repeated_stop_in_the_same_round_goes_through() {
        let refused = format!("PreToolUse hook error: [{RULE_ID}] /loop is recurring");
        let transcript = loop_command() + &wakeup(&stop()) + &result(&refused);
        assert_eq!(decide("ScheduleWakeup", &stop(), &transcript, true), None);
    }

    #[test]
    fn a_refusal_in_an_earlier_round_does_not_excuse_a_later_one() {
        let refused = format!("hook error: [{RULE_ID}] /loop is recurring");
        let transcript = loop_command() + &wakeup(&stop()) + &result(&refused) + &loop_command();
        assert!(decide("ScheduleWakeup", &stop(), &transcript, true).is_some());
    }

    #[test]
    fn the_guard_stays_out_of_claude_routes_other_tools_and_scheduling_calls() {
        let transcript = loop_command();
        let schedule = json!({"delaySeconds": 60, "prompt": "/loop x", "reason": "r"});
        assert_eq!(decide("ScheduleWakeup", &stop(), &transcript, false), None);
        assert_eq!(decide("Bash", &stop(), &transcript, true), None);
        assert_eq!(decide("ScheduleWakeup", &schedule, &transcript, true), None);
    }

    #[test]
    fn without_a_loop_command_or_a_transcript_the_stop_goes_through() {
        assert_eq!(decide("ScheduleWakeup", &stop(), "", true), None);
        assert_eq!(reason("ScheduleWakeup", Some(&stop()), || None, true), None);
    }

    #[test]
    fn the_route_context_names_the_provider() {
        let codex = r#"{"model_provider":"codex","harness":"claude"}"#;
        assert!(non_claude_route(Some(codex)));
        assert!(!non_claude_route(Some(r#"{"model_provider":"claude"}"#)));
        assert!(!non_claude_route(Some("not json")));
        assert!(!non_claude_route(None));
    }
}
