//! Native, forked, or portable resume: the decision table and the recovery payload (#922).
//!
//! `native` hands the session back to Claude's own `--resume`. `portable`
//! starts a fresh session whose first context is rebuilt from the selected
//! transcript: an explicit incomplete-history marker, the compact summary, and
//! the newest whole turns that fit the destination model's budget. `auto`
//! picks native when that is safe and portable when it is not.

use serde::{Deserialize, Serialize};

use super::index::{Lineage, Route};
use super::transcript::{content_text, estimate_tokens, Record, Transcript};

/// The first line of every portable recovery context.
pub const RECOVERY_MARKER: &str = "[CLUD RECOVERY CHECKPOINT — INCOMPLETE HISTORY]";

/// Fraction of the destination window a recovered context may fill, so the
/// resumed agent keeps room to work and to compact again.
pub const BUDGET_FRACTION: f64 = 0.5;

/// Window assumed when the destination model's capability is unknown.
pub const UNKNOWN_MODEL_WINDOW: u64 = 200_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ResumeMode {
    #[default]
    Auto,
    Native,
    Portable,
}

/// What the launch will do with the selected session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumePlan {
    /// `claude --resume <id>`.
    Native { session_id: String },
    /// `claude --resume <id> --fork-session`: a structurally compatible
    /// provider switch that keeps the original untouched.
    ForkedNative { session_id: String },
    /// A fresh session seeded by a recovery payload.
    Portable {
        from_session: String,
        checkpoint: Option<String>,
    },
}

/// Everything the decision table looks at.
#[derive(Debug, Clone)]
pub struct Decision<'a> {
    pub mode: ResumeMode,
    pub session_id: &'a str,
    pub source_route: &'a Route,
    /// `Some` when the user passed an explicit provider flag.
    pub requested_route: Option<&'a Route>,
    /// A compact checkpoint the user picked, if any.
    pub checkpoint: Option<&'a str>,
    /// Estimated tokens a native resume would replay.
    pub resume_tokens: u64,
    /// Budget for the destination model (see [`budget_for_window`]).
    pub budget_tokens: u64,
}

/// Token budget for a destination context window.
pub fn budget_for_window(window: Option<u32>) -> u64 {
    let window = window.map_or(UNKNOWN_MODEL_WINDOW, u64::from);
    (window as f64 * BUDGET_FRACTION) as u64
}

/// A route whose conversation state lives partly outside Claude's transcript
/// (the Codex bridge keeps canonical Responses history in process). Such a
/// session cannot be resumed natively after that process is gone.
pub fn needs_bridge_private_state(route: &Route) -> bool {
    matches!(route, Route::ViaClaude(p) if p == "Codex" || p == "Unified gateway")
}

pub fn decide(decision: &Decision) -> Result<ResumePlan, String> {
    let switching = decision
        .requested_route
        .is_some_and(|requested| requested != decision.source_route);
    let bridge_state = needs_bridge_private_state(decision.source_route)
        || decision
            .requested_route
            .is_some_and(needs_bridge_private_state);
    let oversized = decision.resume_tokens > decision.budget_tokens;
    let portable = || ResumePlan::Portable {
        from_session: decision.session_id.to_string(),
        checkpoint: decision.checkpoint.map(str::to_string),
    };
    match decision.mode {
        ResumeMode::Portable => Ok(portable()),
        ResumeMode::Native => {
            if decision.checkpoint.is_some() {
                return Err(
                    "a compact checkpoint can only be resumed with --resume-mode portable or auto"
                        .to_string(),
                );
            }
            if bridge_state {
                return Err(format!(
                    "{} sessions keep conversation state outside Claude's transcript; resume with --resume-mode portable",
                    decision.source_route.label()
                ));
            }
            if oversized {
                return Err(format!(
                    "the selected session (~{} tokens) exceeds the destination budget of {} tokens; use --resume-mode portable",
                    decision.resume_tokens, decision.budget_tokens
                ));
            }
            Ok(if switching {
                ResumePlan::ForkedNative {
                    session_id: decision.session_id.to_string(),
                }
            } else {
                ResumePlan::Native {
                    session_id: decision.session_id.to_string(),
                }
            })
        }
        ResumeMode::Auto => {
            if decision.checkpoint.is_some() || bridge_state || oversized {
                Ok(portable())
            } else if switching {
                Ok(ResumePlan::ForkedNative {
                    session_id: decision.session_id.to_string(),
                })
            } else {
                Ok(ResumePlan::Native {
                    session_id: decision.session_id.to_string(),
                })
            }
        }
    }
}

/// A turn: one real user prompt and everything up to the next one.
#[derive(Debug, Clone)]
struct Turn {
    text: String,
    tokens: u64,
}

fn is_prompt(record: &Record) -> bool {
    if record.kind != "user" || record.is_meta || record.is_compact_summary {
        return false;
    }
    // A user record carrying only tool results continues the assistant's turn.
    match &record.content {
        serde_json::Value::Array(blocks) => !blocks
            .iter()
            .all(|b| b.get("type").and_then(serde_json::Value::as_str) == Some("tool_result")),
        _ => true,
    }
}

fn turns(records: &[&Record]) -> Vec<Turn> {
    let mut turns: Vec<Vec<&Record>> = Vec::new();
    for record in records {
        if record.kind != "user" && record.kind != "assistant" {
            continue;
        }
        if is_prompt(record) || turns.is_empty() {
            turns.push(Vec::new());
        }
        turns
            .last_mut()
            .expect("a turn was just pushed")
            .push(record);
    }
    turns
        .into_iter()
        .map(|records| {
            let text = records
                .iter()
                .filter_map(|r| {
                    let body = content_text(&r.content);
                    let body = body.trim();
                    if body.is_empty() || (r.kind == "user" && !is_prompt(r)) {
                        return None;
                    }
                    let who = if r.kind == "user" {
                        "User"
                    } else {
                        "Assistant"
                    };
                    Some(format!("{who}: {body}"))
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            let tokens = estimate_tokens(&text);
            Turn { text, tokens }
        })
        .filter(|turn| !turn.text.is_empty())
        .collect()
}

/// A built recovery context plus the lineage to record.
#[derive(Debug, Clone)]
pub struct Recovery {
    pub context: String,
    pub lineage: Lineage,
    pub tokens: u64,
}

/// Build the portable recovery context for `transcript`.
///
/// Starts at `checkpoint` (by uuid) or the newest checkpoint on the active
/// branch; keeps the compact summary and the newest *whole* turns that fit
/// `budget_tokens`, in chronological order; renders tool activity as notes.
pub fn build_recovery(
    transcript: &Transcript,
    from_session: &str,
    checkpoint: Option<&str>,
    budget_tokens: u64,
) -> Result<Recovery, String> {
    let ancestry = transcript.active_ancestry();
    if ancestry.is_empty() {
        return Err("the selected transcript has no conversation to recover".to_string());
    }
    let checkpoints = transcript.checkpoints();
    let chosen = match checkpoint {
        Some(uuid) => Some(
            checkpoints
                .iter()
                .find(|c| c.uuid == uuid)
                .ok_or_else(|| format!("checkpoint {uuid} is not on the active branch"))?,
        ),
        None => checkpoints.last(),
    };
    let start = chosen.map_or(0, |c| c.position + 1);

    let header = format!(
        "{RECOVERY_MARKER}\nThis session was recovered by clud from Claude session {from_session}. \
         Earlier history was truncated: what follows is a compact summary and the most recent \
         complete turns, not the full conversation. Ask before relying on details that are not here."
    );
    let summary = chosen
        .map(|c| format!("## Compact summary\n\n{}", c.summary.trim()))
        .unwrap_or_default();
    let fixed_tokens = estimate_tokens(&header) + estimate_tokens(&summary);
    if fixed_tokens > budget_tokens {
        return Err(format!(
            "even the compact summary (~{fixed_tokens} tokens) exceeds the destination budget of {budget_tokens} tokens"
        ));
    }

    let all_turns = turns(&ancestry[start..]);
    let mut remaining = budget_tokens - fixed_tokens;
    let mut kept: Vec<&Turn> = Vec::new();
    for turn in all_turns.iter().rev() {
        if turn.tokens > remaining {
            break;
        }
        remaining -= turn.tokens;
        kept.push(turn);
    }
    kept.reverse();
    let dropped_turn_tokens: u64 = all_turns.iter().map(|t| t.tokens).sum::<u64>()
        - kept.iter().map(|t| t.tokens).sum::<u64>();
    let before_checkpoint: u64 = ancestry[..start]
        .iter()
        .map(|r| estimate_tokens(&content_text(&r.content)))
        .sum();

    let mut sections = vec![header];
    if !summary.is_empty() {
        sections.push(summary);
    }
    if !kept.is_empty() {
        sections.push(format!(
            "## Most recent turns\n\n{}",
            kept.iter()
                .map(|t| t.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
        ));
    }
    let context = sections.join("\n\n");
    let tokens = estimate_tokens(&context);
    Ok(Recovery {
        context,
        tokens,
        lineage: Lineage {
            recovered_from: from_session.to_string(),
            checkpoint: chosen.map(|c| c.uuid.clone()),
            truncated_tokens: dropped_turn_tokens + before_checkpoint,
        },
    })
}

#[cfg(test)]
#[path = "recover_tests.rs"]
mod tests;
