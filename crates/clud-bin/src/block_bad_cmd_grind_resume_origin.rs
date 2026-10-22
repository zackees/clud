//! Native PostToolUse receipts bind resume admission to the originating cache.

use std::io::{Read, Write};
use std::path::PathBuf;

use serde_json::{json, Value};

use super::{HookPayloadView, MIN_VERSION};

const MAX_RECEIPT: u64 = 8192;

fn path(payload: &HookPayloadView, run: &str) -> Option<PathBuf> {
    if !crate::grind_facts::valid_session_id(run) {
        return None;
    }
    let directory = payload.transcript_path.as_ref()?.with_extension("");
    if !std::fs::symlink_metadata(&directory)
        .ok()?
        .file_type()
        .is_dir()
    {
        return None;
    }
    Some(directory.join(format!("grind-resume-{run}.json")))
}

pub(super) fn record(payload: &HookPayloadView, raw: &Value, version: [u64; 3]) -> Option<()> {
    if version < MIN_VERSION {
        return None;
    }
    let response = raw.get("tool_response")?;
    let run = response.get("runId")?.as_str()?;
    let script = response.get("scriptPath")?.as_str()?;
    let receipt = json!({"schema": "grind-native-cache/v1", "run": run,
        "session": payload.session_id, "script": script, "version": version});
    let bytes = serde_json::to_vec(&receipt).ok()?;
    if bytes.len() as u64 > MAX_RECEIPT {
        return None;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path(payload, run)?)
        .ok()?;
    file.write_all(&bytes).ok()?;
    Some(())
}

pub(super) fn allows(payload: &HookPayloadView) -> bool {
    read(payload).is_some_and(|receipt| {
        let input = payload.tool_input.as_ref();
        let version: Option<[u64; 3]> = receipt
            .get("version")
            .and_then(|version| serde_json::from_value(version.clone()).ok());
        receipt.get("schema").and_then(Value::as_str) == Some("grind-native-cache/v1")
            && receipt.get("session").and_then(Value::as_str) == payload.session_id.as_deref()
            && receipt.get("run") == input.and_then(|input| input.get("resumeFromRunId"))
            && receipt.get("script") == input.and_then(|input| input.get("scriptPath"))
            && version.is_some_and(|version| version >= MIN_VERSION)
    })
}

fn read(payload: &HookPayloadView) -> Option<Value> {
    let run = payload
        .tool_input
        .as_ref()?
        .get("resumeFromRunId")?
        .as_str()?;
    let path = path(payload, run)?;
    if !std::fs::symlink_metadata(&path).ok()?.file_type().is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_RECEIPT + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_RECEIPT {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}
