//! Durable per-launch diagnostics for `clud ui`.
//!
//! The live session-cap registry intentionally deletes rows on graceful exit.
//! These records are separate: one JSON file per launch under the daemon state
//! directory, retained long enough for the dashboard to explain recent exits.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::command::LaunchPlan;
use crate::loop_spec;

const DIR_NAME: &str = "launches";
const MAX_RECORDS: usize = 200;
/// Hard cap on `failure_reason`, in characters. Mirrors the 200-char stderr
/// tail `tool_run` already keeps: long enough for a backend's one-line
/// complaint, short enough that no response body can hide inside it.
const MAX_FAILURE_REASON_CHARS: usize = 200;
/// Substrings that mark a word as credential-shaped. `bridge_log`'s contract
/// keeps bodies, credentials and URLs off disk; this record inherits it, so a
/// matching word is replaced wholesale rather than partially masked.
const SECRET_MARKERS: [&str; 8] = [
    "token",
    "secret",
    "password",
    "authorization",
    "bearer",
    "api_key",
    "apikey",
    "credential",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchRecord {
    pub id: String,
    pub source: String,
    pub clud_pid: u32,
    pub backend: String,
    pub launch_mode: String,
    pub cwd: Option<String>,
    pub repo_root: Option<String>,
    pub command: Vec<String>,
    pub clud_argv: Vec<String>,
    pub launched_at_ms: u64,
    #[serde(default)]
    pub exited_at_ms: Option<u64>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// Why the launch failed, when clud can tell (#998). Bounded and redacted
    /// on the same terms as `bridge_log`: no bodies, no credentials, no URLs.
    /// Absent on a clean exit, and absent from records written before this
    /// field existed -- which `serde(default)` keeps readable.
    #[serde(default)]
    pub failure_reason: Option<String>,
}

impl LaunchRecord {
    pub fn duration_ms(&self) -> Option<u64> {
        self.exited_at_ms
            .map(|end| end.saturating_sub(self.launched_at_ms))
    }
}

#[derive(Debug)]
pub struct LaunchLogHandle {
    state_dir: PathBuf,
    id: String,
}

impl LaunchLogHandle {
    pub fn finish(&self, exit_code: i32, failure_reason: Option<String>) {
        if let Err(err) = finish_launch(&self.state_dir, &self.id, exit_code, failure_reason) {
            eprintln!("[clud] warning: failed to record launch exit: {err}");
        }
    }
}

pub fn start_launch(
    state_dir: &Path,
    plan: &LaunchPlan,
    source: &str,
) -> io::Result<LaunchLogHandle> {
    let launched_at_ms = unix_millis_now();
    let id = format!("{launched_at_ms}-{}", std::process::id());
    let cwd = launch_cwd(plan);
    let repo_root = cwd.as_deref().and_then(repo_root_for_cwd);
    let record = LaunchRecord {
        id: id.clone(),
        source: source.to_string(),
        clud_pid: std::process::id(),
        backend: plan.backend.executable_name().to_string(),
        launch_mode: plan.launch_mode.as_str().to_string(),
        cwd,
        repo_root,
        command: plan.command.clone(),
        clud_argv: std::env::args().collect(),
        launched_at_ms,
        exited_at_ms: None,
        exit_code: None,
        failure_reason: None,
    };
    write_record(state_dir, &record)?;
    prune_old_records(state_dir);
    Ok(LaunchLogHandle {
        state_dir: state_dir.to_path_buf(),
        id,
    })
}

pub fn finish_launch(
    state_dir: &Path,
    id: &str,
    exit_code: i32,
    failure_reason: Option<String>,
) -> io::Result<()> {
    let path = record_path(state_dir, id);
    let bytes = fs::read(&path)?;
    let mut record: LaunchRecord = serde_json::from_slice(&bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    record.exited_at_ms = Some(unix_millis_now());
    record.exit_code = Some(exit_code);
    // Failure path only: a clean exit must never carry a reason. This is also
    // the single choke point where every caller's text is bounded and redacted,
    // so a caller may hand over an untrimmed capture.
    record.failure_reason = if exit_code == 0 {
        None
    } else {
        failure_reason.as_deref().and_then(sanitize_failure_reason)
    };
    write_record(state_dir, &record)
}

/// Reduce raw diagnostic text to a record-safe `failure_reason`.
///
/// Keeps the last non-empty line -- a failing child's complaint is the last
/// thing it says -- strips ANSI escapes, replaces URL- and credential-shaped
/// words, and caps the result at [`MAX_FAILURE_REASON_CHARS`]. Returns `None`
/// when nothing legible survives, so an empty capture cannot manufacture a
/// reason for a launch that has none.
pub fn sanitize_failure_reason(raw: &str) -> Option<String> {
    let plain = strip_ansi(raw);
    let line = plain
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let mut out = String::new();
    for word in line.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        if word.contains("://") {
            out.push_str("[url]");
        } else if is_secret_shaped(word) {
            out.push_str("[redacted]");
        } else {
            out.push_str(word);
        }
    }
    if out.is_empty() {
        return None;
    }
    if out.chars().count() > MAX_FAILURE_REASON_CHARS {
        out = out
            .chars()
            .take(MAX_FAILURE_REASON_CHARS - 1)
            .chain(std::iter::once('\u{2026}'))
            .collect();
    }
    Some(out)
}

/// The classification for the #995 failure class: a bridge-routed launch that
/// exited non-zero without ever asking the bridge for a turn.
///
/// `bridge_log` records upstream failures and retry decisions only, so a launch
/// that dies before issuing a request leaves no `bridge.jsonl` at all -- which
/// is why the original model wedge left nothing but `"exit_code": 1`. "The
/// harness never asked the gateway for a turn" is the fact that separates that
/// case from an upstream fault, and the bridge can answer it. `None` on a clean
/// exit, on a launch that is not bridge-routed, and when the harness did reach
/// the bridge -- `bridge.jsonl` owns that story.
pub fn silent_bridge_reason(turn_requests: Option<usize>, exit_code: i32) -> Option<String> {
    if exit_code == 0 || turn_requests != Some(0) {
        return None;
    }
    Some(format!(
        "backend exited {exit_code} without sending the clud bridge a single \
         message request; it failed before any upstream call"
    ))
}

fn is_secret_shaped(word: &str) -> bool {
    let lowered = word.to_ascii_lowercase();
    SECRET_MARKERS.iter().any(|marker| lowered.contains(marker))
        // An unbroken run this long inside a diagnostic line is a key or a body
        // fragment, never a sentence.
        || word.len() > 64
}

/// Drop ANSI escape sequences and stray control bytes.
///
/// A backend that renders a TUI leaves CSI (`ESC [ ... final`) and OSC
/// (`ESC ] ... BEL`) sequences in anything captured from it. They carry no
/// diagnostic value and would corrupt the record's shape.
fn strip_ansi(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            if ch == '\n' || ch == '\r' || !ch.is_control() {
                out.push(ch);
            }
            continue;
        }
        match chars.next() {
            Some('[') => {
                // CSI terminates on its final byte (0x40..=0x7e).
                for ch in chars.by_ref() {
                    if ch.is_ascii_alphabetic() || ch == '~' {
                        break;
                    }
                }
            }
            Some(']') => {
                // OSC terminates on BEL or the ESC of a string terminator.
                for ch in chars.by_ref() {
                    if ch == '\u{7}' || ch == '\u{1b}' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

pub fn read_recent(state_dir: &Path) -> Vec<LaunchRecord> {
    let dir = launches_dir(state_dir);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut records = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(record) = serde_json::from_slice::<LaunchRecord>(&bytes) else {
            continue;
        };
        records.push(record);
    }
    records.sort_by(|a, b| b.launched_at_ms.cmp(&a.launched_at_ms));
    records.truncate(MAX_RECORDS);
    records
}

pub fn unix_millis_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn launch_cwd(plan: &LaunchPlan) -> Option<String> {
    plan.cwd.clone().or_else(|| {
        std::env::current_dir()
            .ok()
            .map(|path| path.display().to_string())
    })
}

pub fn repo_root_for_cwd(cwd: &str) -> Option<String> {
    let cwd = PathBuf::from(cwd);
    let root = loop_spec::git_root_from(&cwd);
    if root.join(".git").exists() {
        Some(root.display().to_string())
    } else {
        None
    }
}

fn write_record(state_dir: &Path, record: &LaunchRecord) -> io::Result<()> {
    let path = record_path(state_dir, &record.id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(record).map_err(io::Error::other)?;
    fs::write(path, bytes)
}

fn launches_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(DIR_NAME)
}

fn record_path(state_dir: &Path, id: &str) -> PathBuf {
    launches_dir(state_dir).join(format!("{id}.json"))
}

fn prune_old_records(state_dir: &Path) {
    let dir = launches_dir(state_dir);
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    let mut paths: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    if paths.len() <= MAX_RECORDS {
        return;
    }
    paths.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    });
    let remove_count = paths.len().saturating_sub(MAX_RECORDS);
    for path in paths.into_iter().take(remove_count) {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records written before #998 have no `failure_reason` key at all. The
    /// dashboard reads every retained record, so losing them to a decode error
    /// would be a worse regression than the missing field ever was.
    #[test]
    fn a_record_without_failure_reason_still_deserializes() {
        let legacy = r#"{
            "id": "1787016134612-46836",
            "source": "direct",
            "clud_pid": 46836,
            "backend": "claude",
            "launch_mode": "subprocess",
            "cwd": null,
            "repo_root": null,
            "command": ["claude", "--model", "clud-claude-codex-sol"],
            "clud_argv": ["clud"],
            "launched_at_ms": 1787016134612,
            "exited_at_ms": 1787016135546,
            "exit_code": 1
        }"#;
        let record: LaunchRecord = serde_json::from_str(legacy).expect("legacy record must decode");
        assert_eq!(record.exit_code, Some(1));
        assert_eq!(record.failure_reason, None);
    }

    #[test]
    fn a_reason_keeps_the_last_line_and_drops_escapes_urls_and_secrets() {
        let raw = "starting up\n\u{1b}[31mThere's an issue with the selected model \
                   (claude-opus-5[1m]). See https://example.test/models \
                   Authorization=abc123\u{1b}[0m\n\n";
        let reason = sanitize_failure_reason(raw).expect("a reason must survive");
        assert!(
            reason.starts_with("There's an issue with the selected model"),
            "{reason}"
        );
        assert!(reason.contains("(claude-opus-5[1m])"), "{reason}");
        assert!(reason.contains("[url]"), "{reason}");
        assert!(!reason.contains("example.test"), "{reason}");
        assert!(reason.contains("[redacted]"), "{reason}");
        assert!(!reason.contains("abc123"), "{reason}");
        assert!(!reason.contains('\u{1b}'), "{reason}");
    }

    #[test]
    fn a_reason_is_capped_and_empty_text_yields_none() {
        let reason = sanitize_failure_reason(&"body ".repeat(200)).expect("a reason must survive");
        assert_eq!(reason.chars().count(), MAX_FAILURE_REASON_CHARS);
        assert!(reason.ends_with('\u{2026}'), "{reason}");
        assert_eq!(sanitize_failure_reason("   \n\n  "), None);
    }

    /// #995's launch: a bridge-routed `claude --model clud-claude-codex-sol`
    /// that exited 1 in 934 ms having sent the bridge nothing. Before #998 the
    /// record said only `exit_code: 1`.
    #[test]
    fn a_bridge_routed_launch_that_never_asked_for_a_turn_is_classified() {
        let reason = silent_bridge_reason(Some(0), 1).expect("silence must be classified");
        assert!(
            reason.contains("without sending the clud bridge"),
            "{reason}"
        );
        // A launch that did reach the bridge, one that has no bridge, and a
        // clean exit are all somebody else's story.
        assert_eq!(silent_bridge_reason(Some(3), 1), None);
        assert_eq!(silent_bridge_reason(None, 1), None);
        assert_eq!(silent_bridge_reason(Some(0), 0), None);
    }

    #[test]
    fn a_clean_exit_never_carries_a_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        let record = LaunchRecord {
            id: "1-2".to_string(),
            source: "direct".to_string(),
            clud_pid: 2,
            backend: "claude".to_string(),
            launch_mode: "subprocess".to_string(),
            cwd: None,
            repo_root: None,
            command: vec!["claude".to_string()],
            clud_argv: vec!["clud".to_string()],
            launched_at_ms: 1,
            exited_at_ms: None,
            exit_code: None,
            failure_reason: None,
        };
        write_record(dir.path(), &record).expect("write");

        finish_launch(
            dir.path(),
            "1-2",
            0,
            Some("noise from a clean run".to_string()),
        )
        .expect("finish");
        let clean = read_recent(dir.path()).pop().expect("record");
        assert_eq!(clean.failure_reason, None);

        finish_launch(
            dir.path(),
            "1-2",
            1,
            Some("model resolution failed".to_string()),
        )
        .expect("finish");
        let failed = read_recent(dir.path()).pop().expect("record");
        assert_eq!(
            failed.failure_reason.as_deref(),
            Some("model resolution failed")
        );
    }
}
