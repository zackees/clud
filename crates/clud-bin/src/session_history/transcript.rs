//! Read-only view of a Claude Code transcript (`~/.claude/projects/<slug>/<session>.jsonl`).
//!
//! Claude appends one JSON record per line. Records form a tree through
//! `uuid`/`parentUuid`: editing a prompt or rewinding starts a new branch, so
//! file order is not conversation order. Everything here follows the *active
//! ancestry* — the chain from the newest leaf back through `parentUuid` —
//! and never slices raw lines (#922). Compaction writes a `compact_boundary`
//! system record followed by a user record flagged `isCompactSummary` that
//! carries the summary; those are the checkpoints recovery can start from.
//!
//! The transcript is Claude-owned: nothing in this module writes to it.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

/// One parsed transcript line. Only the fields recovery needs are kept.
#[derive(Debug, Clone)]
pub struct Record {
    pub uuid: Option<String>,
    pub parent_uuid: Option<String>,
    /// `user`, `assistant`, `system`, `summary`, ...
    pub kind: String,
    pub subtype: Option<String>,
    pub timestamp: Option<String>,
    pub cwd: Option<String>,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub is_compact_summary: bool,
    pub is_sidechain: bool,
    pub is_meta: bool,
    /// `summary` records name a branch: the title and the leaf it labels.
    pub summary: Option<String>,
    pub leaf_uuid: Option<String>,
    /// `message.content`, as written.
    pub content: Value,
}

impl Record {
    pub fn from_value(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let text = |key: &str| object.get(key).and_then(Value::as_str).map(str::to_string);
        let flag = |key: &str| object.get(key).and_then(Value::as_bool).unwrap_or(false);
        let message = object.get("message");
        Some(Self {
            uuid: text("uuid"),
            parent_uuid: text("parentUuid"),
            kind: text("type").unwrap_or_default(),
            subtype: text("subtype"),
            timestamp: text("timestamp"),
            cwd: text("cwd"),
            session_id: text("sessionId"),
            model: message
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
                .map(str::to_string),
            is_compact_summary: flag("isCompactSummary"),
            is_sidechain: flag("isSidechain"),
            is_meta: flag("isMeta"),
            summary: text("summary"),
            leaf_uuid: text("leafUuid"),
            content: message
                .and_then(|m| m.get("content"))
                .cloned()
                .unwrap_or(Value::Null),
        })
    }

    fn is_turn(&self) -> bool {
        matches!(self.kind.as_str(), "user" | "assistant") && !self.is_sidechain
    }
}

/// A whole transcript, in file order.
#[derive(Debug, Clone, Default)]
pub struct Transcript {
    pub records: Vec<Record>,
    /// Lines that were not valid JSON (a torn final write, usually).
    pub skipped_lines: usize,
}

impl Transcript {
    pub fn parse(text: &str) -> Self {
        let mut transcript = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(line)
                .ok()
                .as_ref()
                .and_then(Record::from_value)
            {
                Some(record) => transcript.records.push(record),
                None => transcript.skipped_lines += 1,
            }
        }
        transcript
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        Ok(Self::parse(&std::fs::read_to_string(path)?))
    }

    pub fn session_id(&self) -> Option<&str> {
        self.records.iter().find_map(|r| r.session_id.as_deref())
    }

    pub fn cwd(&self) -> Option<&str> {
        self.records.iter().rev().find_map(|r| r.cwd.as_deref())
    }

    pub fn last_timestamp(&self) -> Option<&str> {
        self.records
            .iter()
            .rev()
            .find_map(|r| r.timestamp.as_deref())
    }

    /// The model of the newest assistant turn on the active branch.
    pub fn model(&self) -> Option<String> {
        self.active_ancestry()
            .iter()
            .rev()
            .find_map(|r| r.model.clone())
    }

    /// The newest leaf: the last main-chain user/assistant record in file order.
    fn leaf(&self) -> Option<&Record> {
        self.records
            .iter()
            .rev()
            .find(|r| r.is_turn() && r.uuid.is_some())
    }

    /// The active branch, oldest first: the newest leaf and its `parentUuid`
    /// ancestors. Stops at a missing parent (the start, or a compaction
    /// boundary whose parent chain is cut) and at cycles.
    pub fn active_ancestry(&self) -> Vec<&Record> {
        let by_uuid: HashMap<&str, &Record> = self
            .records
            .iter()
            .filter_map(|r| r.uuid.as_deref().map(|id| (id, r)))
            .collect();
        let mut chain = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut cursor = self.leaf();
        while let Some(record) = cursor {
            let Some(id) = record.uuid.as_deref() else {
                break;
            };
            if !seen.insert(id) {
                break;
            }
            chain.push(record);
            cursor = record
                .parent_uuid
                .as_deref()
                .and_then(|parent| by_uuid.get(parent).copied());
        }
        chain.reverse();
        chain
    }

    /// Branch title: the `summary` record naming the active leaf (or any
    /// ancestor), else the newest `summary` record.
    pub fn title(&self) -> Option<String> {
        let ancestry: std::collections::HashSet<&str> = self
            .active_ancestry()
            .iter()
            .filter_map(|r| r.uuid.as_deref())
            .collect();
        let mut summaries = self.records.iter().filter(|r| r.kind == "summary");
        summaries
            .clone()
            .rev()
            .find(|r| r.leaf_uuid.as_deref().is_some_and(|l| ancestry.contains(l)))
            .or_else(|| summaries.next_back())
            .and_then(|r| r.summary.clone())
    }

    /// The first real prompt on the active branch, single-lined and cut to
    /// `max_chars`, for a picker row when there is no title.
    pub fn preview(&self, max_chars: usize) -> Option<String> {
        let prompt = self
            .active_ancestry()
            .into_iter()
            .filter(|r| r.kind == "user" && !r.is_meta && !r.is_compact_summary)
            .find_map(|r| {
                let text = content_text(&r.content);
                let text = text.trim();
                (!text.is_empty() && !text.starts_with('<')).then(|| text.to_string())
            })?;
        Some(sanitize_preview(&prompt, max_chars))
    }

    /// Compact checkpoints on the active branch, oldest first.
    pub fn checkpoints(&self) -> Vec<Checkpoint> {
        self.active_ancestry()
            .iter()
            .enumerate()
            .filter(|(_, r)| r.is_compact_summary)
            .map(|(position, r)| Checkpoint {
                uuid: r.uuid.clone().unwrap_or_default(),
                position,
                timestamp: r.timestamp.clone(),
                summary: content_text(&r.content),
            })
            .collect()
    }
}

/// A compaction point on the active branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub uuid: String,
    /// Index in [`Transcript::active_ancestry`].
    pub position: usize,
    pub timestamp: Option<String>,
    pub summary: String,
}

/// Flatten `message.content` to text. Text blocks are kept verbatim; tool
/// calls and results become one-line notes so recovery never emits a
/// dangling tool protocol record (#922).
pub fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                let kind = block.get("type").and_then(Value::as_str)?;
                match kind {
                    "text" => block
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    "tool_use" => {
                        let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                        Some(format!("[used tool {name}]"))
                    }
                    "tool_result" => Some("[tool result omitted]".to_string()),
                    _ => None,
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// One line, control characters removed, at most `max_chars` characters.
pub fn sanitize_preview(text: &str, max_chars: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        flat
    } else {
        let cut: String = flat.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// Conservative token estimate for text: about one token per three bytes.
/// Deliberately pessimistic, so a budget computed from it leaves headroom.
pub fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(3)
}

/// Estimated tokens of the context a session would resume with: the active
/// branch from its newest compact checkpoint onward (a native resume only
/// replays that far), never the cumulative API usage fields (#922).
pub fn estimate_resume_tokens(transcript: &Transcript) -> u64 {
    let ancestry = transcript.active_ancestry();
    let start = transcript
        .checkpoints()
        .last()
        .map_or(0, |checkpoint| checkpoint.position);
    ancestry[start..]
        .iter()
        .map(|r| estimate_tokens(&content_text(&r.content)))
        .sum()
}

#[cfg(test)]
#[path = "transcript_tests.rs"]
pub(crate) mod tests;
