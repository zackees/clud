//! #1067 step 2: measure what the command gate would cost before turning it
//! on anywhere.
//!
//! A dogfood period only sees sessions that already ran gated. Replaying the
//! Bash commands agents actually wrote in ungated sessions answers the
//! question first: how many would the gate admit as written, how many need
//! only the `tap` prefix, and how many need restructuring. It runs the gate's
//! own [`crate::block_bad_cmd::classify_for_gate`], so the numbers cannot
//! drift from the real decision.
//!
//! Content-safe by construction: the report holds counts and the gate's fixed
//! construct names, never command text, paths, or prompts.

use std::collections::BTreeMap;

use crate::block_bad_cmd::{classify_for_gate, GateClass};

/// Every Bash command in one Claude Code transcript line. A line is one JSON
/// record; assistant records carry `message.content[]` items, and a Bash
/// tool call is `{"type":"tool_use","name":"Bash","input":{"command":..}}`.
/// Anything else, including a malformed line, yields nothing.
pub fn bash_commands(line: &str) -> Vec<String> {
    let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
        return Vec::new();
    };
    let Some(items) = record
        .pointer("/message/content")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|item| {
            item.get("type").and_then(serde_json::Value::as_str) == Some("tool_use")
                && item.get("name").and_then(serde_json::Value::as_str) == Some("Bash")
        })
        .filter_map(|item| item.pointer("/input/command")?.as_str())
        .map(str::to_string)
        .collect()
}

/// Aggregate counts for a replay.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GateReplayReport {
    pub transcripts: usize,
    pub commands: usize,
    pub allowed: usize,
    pub prefix_single: usize,
    pub prefix_compound: usize,
    /// Commands the gate refuses even when fully wrapped, by construct.
    pub restructure: BTreeMap<&'static str, usize>,
}

impl GateReplayReport {
    pub fn record(&mut self, command: &str, prefix: &str) {
        self.commands += 1;
        match classify_for_gate(command, prefix) {
            GateClass::Allowed => self.allowed += 1,
            GateClass::PrefixSingle => self.prefix_single += 1,
            GateClass::PrefixCompound => self.prefix_compound += 1,
            GateClass::Restructure(construct) => {
                *self.restructure.entry(construct).or_default() += 1;
            }
        }
    }

    pub fn restructure_total(&self) -> usize {
        self.restructure.values().sum()
    }

    /// The share of commands that would be refused even after an agent adds
    /// the prefix everywhere -- the gate's irreducible friction.
    pub fn restructure_rate(&self) -> f64 {
        if self.commands == 0 {
            0.0
        } else {
            self.restructure_total() as f64 / self.commands as f64
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        let share = |count: usize| {
            if self.commands == 0 {
                0.0
            } else {
                (count as f64 / self.commands as f64 * 1000.0).round() / 10.0
            }
        };
        serde_json::json!({
            "transcripts": self.transcripts,
            "commands": self.commands,
            "allowed": {"count": self.allowed, "percent": share(self.allowed)},
            "prefix_single": {"count": self.prefix_single, "percent": share(self.prefix_single)},
            "prefix_compound": {"count": self.prefix_compound, "percent": share(self.prefix_compound)},
            "restructure": {
                "count": self.restructure_total(),
                "percent": share(self.restructure_total()),
                "by_construct": self.restructure,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_bash_tool_calls() {
        let line = r#"{"type":"assistant","message":{"content":[
            {"type":"text","text":"rm -rf / is bad"},
            {"type":"tool_use","name":"Bash","input":{"command":"ls -la"}},
            {"type":"tool_use","name":"Read","input":{"file_path":"/x"}},
            {"type":"tool_use","name":"Bash","input":{"command":"git status && git diff"}}
        ]}}"#
            .replace('\n', "");
        assert_eq!(
            bash_commands(&line),
            vec!["ls -la".to_string(), "git status && git diff".to_string()]
        );
        assert!(bash_commands("not json").is_empty());
        assert!(bash_commands(r#"{"type":"user","message":{"content":"hi"}}"#).is_empty());
    }

    #[test]
    fn report_counts_each_class_and_never_holds_command_text() {
        let mut report = GateReplayReport::default();
        for command in [
            "tap ls",
            "ls -la",
            "git status && git diff",
            "echo $(secret-token-value)",
            "for f in *; do rm $f; done",
        ] {
            report.record(command, "tap");
        }
        assert_eq!(report.commands, 5);
        assert_eq!(report.allowed, 1);
        assert_eq!(report.prefix_single, 1);
        assert_eq!(report.prefix_compound, 1);
        assert_eq!(report.restructure_total(), 2);
        assert!((report.restructure_rate() - 0.4).abs() < 1e-9);
        let rendered = report.to_json().to_string();
        assert!(rendered.contains("command substitution"));
        assert!(rendered.contains("control flow"));
        assert!(!rendered.contains("secret-token-value"));
        assert!(!rendered.contains("git status"));
    }
}
