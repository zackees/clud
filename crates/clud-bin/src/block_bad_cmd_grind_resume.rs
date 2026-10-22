//! Refuse graph resume when the native runtime cannot keep branch-local caches.

use std::io::{Read, Seek, SeekFrom};

use serde_json::Value;

use super::super::HookPayloadView;

const MAX_TRANSCRIPT_TAIL: u64 = 256 * 1024;
const MIN_VERSION: [u64; 3] = [2, 1, 306];

pub(super) fn reason(payload: &HookPayloadView) -> Option<String> {
    if payload.tool_name != "Workflow" {
        return None;
    }
    let input = payload.tool_input.as_ref()?;
    if input.get("resumeFromRunId").is_none_or(Value::is_null) {
        return None;
    }
    let facts = crate::grind_facts::lookup(payload.session_id.as_deref());
    reason_for(payload, facts.facts())
}

fn reason_for(payload: &HookPayloadView, facts: Option<&Value>) -> Option<String> {
    let input = payload.tool_input.as_ref()?;
    let graph = input.pointer("/args/orchestration").is_some_and(Value::is_object)
        || facts.is_some_and(|facts| facts.get("orchestration").is_some_and(Value::is_object));
    if !graph {
        return None;
    }
    let version = transcript_version(payload);
    if version.is_some_and(|version| version >= MIN_VERSION) {
        return None;
    }
    Some("Graph resumption requires Claude Code >= 2.1.306 with branch-local native caches; \
          this session's runtime is older or could not be verified. Keep orchestration_resume \
          and its deferred manifest, report the missing capability, and do not run Finish or \
          replay completed work. Update the harness before explicit resumption."
        .into())
}

fn transcript_version(payload: &HookPayloadView) -> Option<[u64; 3]> {
    let session = payload.session_id.as_deref()?;
    if !crate::grind_facts::valid_session_id(session) {
        return None;
    }
    let path = payload.transcript_path.as_deref()?;
    if !path.is_absolute()
        || path.file_name()? != std::ffi::OsStr::new(&format!("{session}.jsonl"))
    {
        return None;
    }
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    let offset = metadata.len().saturating_sub(MAX_TRANSCRIPT_TAIL);
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_TRANSCRIPT_TAIL).read_to_end(&mut bytes).ok()?;
    // Discard a partial first record before decoding, including a partial UTF-8 character.
    let complete = if offset > 0 {
        &bytes[bytes.iter().position(|byte| *byte == b'\n')? + 1..]
    } else {
        &bytes
    };
    complete.split(|byte| *byte == b'\n').rev().find_map(|line| {
        let row: Value = serde_json::from_slice(line).ok()?;
        if row.get("sessionId").and_then(Value::as_str) != Some(session) {
            return None;
        }
        row.get("version").map(|version| version.as_str().and_then(parse_version))
    })?
}

fn parse_version(value: &str) -> Option<[u64; 3]> {
    let mut parts = value.split('.');
    let version = [
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ];
    parts.next().is_none().then_some(version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(path: &std::path::Path) -> HookPayloadView {
        HookPayloadView {
            tool_name: "Workflow".into(),
            command: String::new(),
            cwd: path.parent().unwrap().into(),
            tool_input: Some(json!({"resumeFromRunId": "wf-prior", "args": {
                "orchestration": {"goals": []}
            }})),
            agent_id: None,
            agent_type: None,
            session_id: Some("session".into()),
            transcript_path: Some(path.into()),
        }
    }

    fn record(version: Value, session: &str) -> String {
        json!({"type": "assistant", "sessionId": session, "version": version}).to_string() + "\n"
    }

    #[test]
    fn rejects_older_unknown_and_malformed_versions_without_changing_manifest() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.jsonl");
        let payload = payload(&path);
        let manifest = json!({"orchestration_resume": {"remaining_goals": ["b", "c"]}});
        for version in [json!("2.1.282"), json!("2.1.305"), json!("unknown"), Value::Null] {
            std::fs::write(&path, record(version, "session")).unwrap();
            let denial = reason_for(&payload, Some(&manifest)).unwrap();
            assert!(denial.contains("2.1.306"));
            assert!(denial.contains("Keep orchestration_resume"));
            assert_eq!(manifest["orchestration_resume"]["remaining_goals"], json!(["b", "c"]));
        }
        std::fs::write(&path, "{}\n").unwrap();
        assert!(reason_for(&payload, None).is_some());
    }

    #[test]
    fn uses_latest_matching_session_version_in_bounded_transcript_tail() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.jsonl");
        let payload = payload(&path);
        for version in ["2.1.306", "2.2.0", "3.0.0"] {
            let content = "x".repeat(MAX_TRANSCRIPT_TAIL as usize + 1) + "\n"
                + &record(json!("2.1.282"), "session")
                + &record(json!(version), "session")
                + &record(json!("0.0.0"), "another-session");
            std::fs::write(&path, content).unwrap();
            assert!(reason_for(&payload, None).is_none());
        }
        std::fs::write(&path, record(json!("2.1.306"), "session")
            + &record(json!("unknown"), "session")).unwrap();
        assert!(reason_for(&payload, None).is_some());
    }

    #[test]
    fn non_graph_resumes_are_unchanged_and_facts_cannot_hide_a_graph() {
        let root = tempfile::tempdir().unwrap();
        let mut payload = payload(&root.path().join("session.jsonl"));
        payload.tool_input = Some(json!({"resumeFromRunId": "wf-prior"}));
        assert!(reason_for(&payload, None).is_none());
        assert!(reason_for(&payload, Some(&json!({"orchestration": {}}))).is_some());
        payload.tool_name = "Bash".into();
        assert!(reason(&payload).is_none());
        payload.tool_name = "Workflow".into();
        payload.tool_input = Some(json!({"name": "grind-run"}));
        assert!(reason(&payload).is_none());
    }
}
