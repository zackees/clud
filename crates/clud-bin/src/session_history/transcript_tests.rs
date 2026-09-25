use super::*;
use serde_json::json;

/// Build a transcript from synthetic records (never real history, #922).
pub(crate) fn transcript_of(records: &[Value]) -> Transcript {
    Transcript::parse(
        &records
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

pub(crate) fn user(uuid: &str, parent: Option<&str>, text: &str) -> Value {
    json!({
        "type": "user", "uuid": uuid, "parentUuid": parent,
        "sessionId": "s-1", "cwd": "/work/proj", "timestamp": format!("2026-01-01T00:00:{uuid}Z"),
        "message": {"role": "user", "content": text}
    })
}

pub(crate) fn assistant(uuid: &str, parent: &str, text: &str) -> Value {
    json!({
        "type": "assistant", "uuid": uuid, "parentUuid": parent,
        "sessionId": "s-1", "cwd": "/work/proj", "timestamp": format!("2026-01-01T00:00:{uuid}Z"),
        "message": {"role": "assistant", "model": "claude-sonnet-4-5",
                    "content": [{"type": "text", "text": text}]}
    })
}

pub(crate) fn compact_summary(uuid: &str, parent: &str, summary: &str) -> Value {
    json!({
        "type": "user", "uuid": uuid, "parentUuid": parent, "isCompactSummary": true,
        "sessionId": "s-1", "cwd": "/work/proj", "timestamp": format!("2026-01-01T00:00:{uuid}Z"),
        "message": {"role": "user", "content": summary}
    })
}

fn ids(records: &[&Record]) -> Vec<String> {
    records.iter().filter_map(|r| r.uuid.clone()).collect()
}

/// A rewind makes a second branch off `02`. File order interleaves the
/// branches; the active ancestry must follow `parentUuid` from the newest
/// leaf, not the order lines were written.
#[test]
fn active_ancestry_follows_parent_links_not_file_order() {
    let transcript = transcript_of(&[
        user("01", None, "first prompt"),
        assistant("02", "01", "first answer"),
        user("03", Some("02"), "abandoned branch"),
        assistant("04", "03", "abandoned answer"),
        user("05", Some("02"), "rewritten prompt"),
        assistant("06", "05", "kept answer"),
    ]);
    assert_eq!(ids(&transcript.active_ancestry()), ["01", "02", "05", "06"]);
}

#[test]
fn sidechain_records_never_become_the_leaf() {
    let mut side = assistant("09", "02", "subagent chatter");
    side["isSidechain"] = json!(true);
    let transcript = transcript_of(&[
        user("01", None, "prompt"),
        assistant("02", "01", "answer"),
        side,
    ]);
    assert_eq!(ids(&transcript.active_ancestry()), ["01", "02"]);
}

#[test]
fn every_compact_checkpoint_on_the_branch_is_indexed_in_order() {
    let transcript = transcript_of(&[
        user("01", None, "prompt"),
        assistant("02", "01", "answer"),
        compact_summary("03", "02", "summary one"),
        assistant("04", "03", "after first compact"),
        compact_summary("05", "04", "summary two"),
        assistant("06", "05", "latest"),
    ]);
    let checkpoints = transcript.checkpoints();
    assert_eq!(
        checkpoints
            .iter()
            .map(|c| c.uuid.as_str())
            .collect::<Vec<_>>(),
        ["03", "05"]
    );
    assert_eq!(checkpoints[1].summary, "summary two");
    assert_eq!(checkpoints[1].position, 4);
}

#[test]
fn a_cycle_or_missing_parent_ends_the_walk_instead_of_looping() {
    let transcript = transcript_of(&[user("01", Some("02"), "a"), assistant("02", "01", "b")]);
    let chain = transcript.active_ancestry();
    assert_eq!(chain.len(), 2, "cycle must terminate");
}

#[test]
fn title_prefers_the_summary_that_labels_the_active_branch() {
    let transcript = transcript_of(&[
        user("01", None, "prompt"),
        assistant("02", "01", "answer"),
        json!({"type": "summary", "summary": "Active title", "leafUuid": "02"}),
        json!({"type": "summary", "summary": "Other branch", "leafUuid": "zz"}),
    ]);
    assert_eq!(transcript.title().as_deref(), Some("Active title"));
}

#[test]
fn preview_is_the_first_real_prompt_single_lined_and_bounded() {
    let mut meta = user("00", None, "<command-name>/clear</command-name>");
    meta["isMeta"] = json!(true);
    let transcript = transcript_of(&[
        meta,
        user("01", Some("00"), "fix the\n\tflaky   test\u{7} please"),
        assistant("02", "01", "ok"),
    ]);
    assert_eq!(
        transcript.preview(100).as_deref(),
        Some("fix the flaky test please")
    );
    assert_eq!(transcript.preview(8).as_deref(), Some("fix the…"));
}

#[test]
fn tool_blocks_flatten_to_notes_so_no_dangling_tool_events_survive() {
    let content = json!([
        {"type": "text", "text": "running it"},
        {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "ls"}},
        {"type": "tool_result", "tool_use_id": "t1", "content": "secret output"}
    ]);
    let text = content_text(&content);
    assert_eq!(text, "running it\n[used tool Bash]\n[tool result omitted]");
    assert!(!text.contains("secret output"));
}

#[test]
fn torn_lines_are_skipped_not_fatal() {
    let transcript = Transcript::parse("{\"type\":\"user\",\"uuid\":\"01\"}\n{\"type\":\"assis");
    assert_eq!(transcript.records.len(), 1);
    assert_eq!(transcript.skipped_lines, 1);
}

/// #922: the estimate is sized from the content a resume would replay (from
/// the newest checkpoint on), never from cumulative `usage` fields.
#[test]
fn resume_estimate_counts_selected_content_not_cumulative_usage() {
    let mut huge_usage = assistant("02", "01", "short");
    huge_usage["message"]["usage"] =
        json!({"input_tokens": 90_000_000, "cache_read_input_tokens": 50_000_000});
    let transcript = transcript_of(&[
        user("01", None, &"x".repeat(3000)),
        huge_usage,
        compact_summary("03", "02", &"s".repeat(300)),
        assistant("04", "03", &"y".repeat(30)),
    ]);
    // Only the checkpoint (300 bytes) and the turn after it (30) count.
    assert_eq!(estimate_resume_tokens(&transcript), 100 + 10);
}
