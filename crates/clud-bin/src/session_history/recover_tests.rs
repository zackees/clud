use super::*;
use crate::session_history::transcript::tests::{assistant, compact_summary, transcript_of, user};
use serde_json::json;

fn decision<'a>(
    mode: ResumeMode,
    source: &'a Route,
    requested: Option<&'a Route>,
    checkpoint: Option<&'a str>,
    tokens: u64,
) -> Decision<'a> {
    Decision {
        mode,
        session_id: "s",
        source_route: source,
        requested_route: requested,
        checkpoint,
        resume_tokens: tokens,
        budget_tokens: 100_000,
    }
}

fn native() -> ResumePlan {
    ResumePlan::Native {
        session_id: "s".into(),
    }
}
fn forked() -> ResumePlan {
    ResumePlan::ForkedNative {
        session_id: "s".into(),
    }
}
fn portable(checkpoint: Option<&str>) -> ResumePlan {
    ResumePlan::Portable {
        from_session: "s".into(),
        checkpoint: checkpoint.map(str::to_string),
    }
}

/// The documented decision table (#922), row by row.
#[test]
fn auto_mode_decision_table() {
    let claude = Route::Claude;
    let deepseek = Route::ViaClaude("DeepSeek".into());
    let codex = Route::ViaClaude("Codex".into());
    // Same provider, within budget: native.
    assert_eq!(
        decide(&decision(ResumeMode::Auto, &claude, None, None, 10)),
        Ok(native())
    );
    // Explicit same provider is not a switch.
    assert_eq!(
        decide(&decision(
            ResumeMode::Auto,
            &claude,
            Some(&claude),
            None,
            10
        )),
        Ok(native())
    );
    // Structurally compatible switch, within budget: forked native.
    assert_eq!(
        decide(&decision(
            ResumeMode::Auto,
            &claude,
            Some(&deepseek),
            None,
            10
        )),
        Ok(forked())
    );
    // Oversized input: portable.
    assert_eq!(
        decide(&decision(ResumeMode::Auto, &claude, None, None, 200_000)),
        Ok(portable(None))
    );
    // Missing bridge-private state (Codex via Claude): portable.
    assert_eq!(
        decide(&decision(ResumeMode::Auto, &codex, None, None, 10)),
        Ok(portable(None))
    );
    // Switching *to* a bridge route: portable.
    assert_eq!(
        decide(&decision(ResumeMode::Auto, &claude, Some(&codex), None, 10)),
        Ok(portable(None))
    );
    // An explicitly selected checkpoint: portable from it.
    assert_eq!(
        decide(&decision(ResumeMode::Auto, &claude, None, Some("c1"), 10)),
        Ok(portable(Some("c1")))
    );
}

#[test]
fn native_mode_refuses_what_it_cannot_do_with_an_actionable_error() {
    let claude = Route::Claude;
    let codex = Route::ViaClaude("Codex".into());
    assert_eq!(
        decide(&decision(ResumeMode::Native, &claude, None, None, 10)),
        Ok(native())
    );
    for (bad, hint) in [
        (
            decision(ResumeMode::Native, &codex, None, None, 10),
            "portable",
        ),
        (
            decision(ResumeMode::Native, &claude, None, None, 200_000),
            "portable",
        ),
        (
            decision(ResumeMode::Native, &claude, None, Some("c1"), 10),
            "portable",
        ),
    ] {
        let error = decide(&bad).unwrap_err();
        assert!(error.contains(hint), "{error}");
    }
}

#[test]
fn portable_mode_always_recovers() {
    let claude = Route::Claude;
    assert_eq!(
        decide(&decision(ResumeMode::Portable, &claude, None, None, 10)),
        Ok(portable(None))
    );
}

#[test]
fn budget_is_half_the_destination_window_with_a_conservative_default() {
    assert_eq!(budget_for_window(Some(1_000_000)), 500_000);
    assert_eq!(budget_for_window(None), 100_000);
}

#[test]
fn recovery_starts_with_the_marker_and_uses_the_selected_checkpoint() {
    let transcript = transcript_of(&[
        user("01", None, "ancient prompt"),
        assistant("02", "01", "ancient answer"),
        compact_summary("03", "02", "FIRST SUMMARY"),
        user("04", Some("03"), "middle prompt"),
        assistant("05", "04", "middle answer"),
        compact_summary("06", "05", "SECOND SUMMARY"),
        user("07", Some("06"), "latest prompt"),
        assistant("08", "07", "latest answer"),
    ]);
    let newest = build_recovery(&transcript, "src", None, 100_000).unwrap();
    assert!(newest.context.starts_with(RECOVERY_MARKER));
    assert!(newest.context.contains("SECOND SUMMARY"));
    assert!(newest.context.contains("User: latest prompt"));
    assert!(!newest.context.contains("middle prompt"));
    assert_eq!(newest.lineage.checkpoint.as_deref(), Some("06"));
    assert_eq!(newest.lineage.recovered_from, "src");

    let first = build_recovery(&transcript, "src", Some("03"), 100_000).unwrap();
    assert!(first.context.contains("FIRST SUMMARY"));
    assert!(first.context.contains("middle prompt"));
    assert!(build_recovery(&transcript, "src", Some("zz"), 100_000).is_err());
}

/// Tool calls and results inside a turn stay in that turn as notes; no tool
/// protocol record (and no tool output) leaks into the recovery context.
#[test]
fn tool_activity_stays_inside_whole_turns_as_notes() {
    let mut tool_call = assistant("02", "01", "");
    tool_call["message"]["content"] = json!([
        {"type": "text", "text": "checking"},
        {"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}
    ]);
    let mut tool_result = user("03", Some("02"), "");
    tool_result["message"]["content"] =
        json!([{"type": "tool_result", "tool_use_id": "t1", "content": "PRIVATE OUTPUT"}]);
    let transcript = transcript_of(&[
        user("01", None, "run it"),
        tool_call,
        tool_result,
        assistant("04", "03", "it passed"),
    ]);
    let recovery = build_recovery(&transcript, "src", None, 100_000).unwrap();
    assert!(recovery.context.contains("[used tool Bash]"));
    assert!(!recovery.context.contains("PRIVATE OUTPUT"));
    assert!(!recovery.context.contains("tool_use_id"));
    // One turn: the tool-result record did not start a new one.
    assert_eq!(recovery.context.matches("User: ").count(), 1);
}

/// #922 acceptance: a ~1M-token source recovered onto a 250k-token model
/// fits the destination's safety budget, keeps the newest turns, and leaves
/// working room.
#[test]
fn a_million_token_source_self_heals_into_a_250k_destination() {
    let mut records = vec![user("0000", None, "start")];
    let mut parent = "0000".to_string();
    // 400 turns of ~2.5k tokens each: ~1M tokens of history.
    for i in 1..=400 {
        let prompt = format!("p{i:04}");
        let answer = format!("a{i:04}");
        records.push(user(
            &prompt,
            Some(&parent),
            &format!("prompt {i} {}", "x".repeat(3_700)),
        ));
        records.push(assistant(
            &answer,
            &prompt,
            &format!("answer {i} {}", "y".repeat(3_700)),
        ));
        parent = answer;
    }
    let transcript = transcript_of(&records);
    assert!(crate::session_history::transcript::estimate_resume_tokens(&transcript) > 900_000);

    let budget = budget_for_window(Some(250_000));
    let recovery = build_recovery(&transcript, "big", None, budget).unwrap();
    assert!(recovery.tokens <= budget, "{} > {budget}", recovery.tokens);
    assert!(recovery.tokens < 250_000 / 2 + 1, "must leave working room");
    assert!(recovery.context.contains("prompt 400 "), "newest turn kept");
    assert!(
        !recovery.context.contains("prompt 1 "),
        "oldest turn dropped"
    );
    assert!(recovery.lineage.truncated_tokens > 700_000);
    // Turns stay whole and in order.
    let first_kept = recovery.context.find("User: prompt").unwrap();
    let last_kept = recovery.context.rfind("User: prompt").unwrap();
    assert!(first_kept < last_kept);
}

#[test]
fn a_summary_bigger_than_the_budget_is_an_actionable_error() {
    let transcript = transcript_of(&[
        user("01", None, "p"),
        assistant("02", "01", "a"),
        compact_summary("03", "02", &"s".repeat(30_000)),
    ]);
    let error = build_recovery(&transcript, "src", None, 1_000).unwrap_err();
    assert!(error.contains("exceeds the destination budget"), "{error}");
}
