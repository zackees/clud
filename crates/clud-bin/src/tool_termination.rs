//! Termination ergonomic + structured exit payload. Slice 6 of #427.
//!
//! When `clud tool run` exits abnormally (timeout, progress-watchdog,
//! non-zero exit) the wrapper prints a human-readable pointer block
//! to stderr that names the session-local integer tool ID and the
//! one-liner follow-up commands the agent (or human) can copy-paste:
//!
//! ```text
//! TIMEOUT after 60m on docker-build (tool #3).
//! For last 50 lines:  clud tool info 3
//! For full log:       clud tool log  3
//! ```
//!
//! The structured exit payload (a single JSON line emitted before the
//! pointer block) includes both the session-local integer and the
//! long-form `<session-pid>-<tool-id>` so downstream tooling can
//! correlate across sessions. The schema is locked here so changes
//! are intentional.

use std::io::{self, Write};
use std::time::Duration;

use serde_json::json;

use crate::session_index::LongFormId;
use crate::tool_watchdog::AbortReason;

/// Discriminates the four exit shapes the wrapper can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitKind {
    /// Normal exit with zero exit code.
    Finished,
    /// Normal exit with non-zero exit code.
    Failed,
    /// Watchdog (command or progress) fired on a killable tool;
    /// the process tree was killed and the wrapper exits 124.
    Aborted(AbortReason),
    /// Watchdog fired on a resumable tool; the process keeps running
    /// from the world's perspective, the wrapper exits 0 with the
    /// in-progress JSON so the caller re-invokes to resume.
    InProgress(AbortReason),
}

impl ExitKind {
    pub fn label(self) -> &'static str {
        match self {
            ExitKind::Finished => "finished",
            ExitKind::Failed => "failed",
            ExitKind::Aborted(_) => "aborted",
            ExitKind::InProgress(_) => "in-progress",
        }
    }

    pub fn is_normal(self) -> bool {
        matches!(self, ExitKind::Finished)
    }
}

/// Structured exit payload. One JSON line, schema-versioned. Emitted
/// to stderr before the human-readable pointer block so callers piping
/// stderr through `jq` can parse the first line as JSON.
#[allow(clippy::too_many_arguments)]
pub fn render_structured_payload(
    session_pid: u32,
    tool_id: u32,
    tool: &str,
    args: &[String],
    started_at_ms: u128,
    ended_at_ms: u128,
    elapsed: Duration,
    exit_kind: ExitKind,
    exit_code: Option<i32>,
) -> String {
    let long = LongFormId {
        session_pid,
        tool_id,
    }
    .format();
    let reason = match exit_kind {
        ExitKind::Aborted(r) | ExitKind::InProgress(r) => Some(r.label()),
        _ => None,
    };
    let value = json!({
        "v": 1,
        "tool_id": tool_id,
        "long_id": long,
        "tool": tool,
        "args": args,
        "started_at_ms": started_at_ms as u64,
        "ended_at_ms": ended_at_ms as u64,
        "elapsed_ms": elapsed.as_millis() as u64,
        "status": exit_kind.label(),
        "reason": reason,
        "exit_code": exit_code,
    });
    value.to_string()
}

/// Human-readable pointer block. Renders to stderr after the JSON
/// payload. Uses the session-local integer ID (the PM2-friendly form)
/// for the follow-up commands so the user types `clud tool info 3`,
/// not `clud tool info 47180-3`.
pub fn render_pointer_block(
    tool_id: u32,
    tool: &str,
    elapsed: Duration,
    exit_kind: ExitKind,
) -> String {
    let header = match exit_kind {
        ExitKind::Finished => format!("OK {} ({}) on {}", tool_id, format_elapsed(elapsed), tool),
        ExitKind::Failed => format!(
            "FAILED on {} (tool #{}) after {}.",
            tool,
            tool_id,
            format_elapsed(elapsed)
        ),
        ExitKind::Aborted(reason) => format!(
            "{} after {} on {} (tool #{}).",
            match reason {
                AbortReason::CommandTimeout => "TIMEOUT",
                AbortReason::ProgressTimeout => "STUCK (no output)",
            },
            format_elapsed(elapsed),
            tool,
            tool_id
        ),
        ExitKind::InProgress(reason) => format!(
            "{} on {} (tool #{}) after {} — RESUMABLE.",
            match reason {
                AbortReason::CommandTimeout => "STILL RUNNING",
                AbortReason::ProgressTimeout => "QUIET",
            },
            tool,
            tool_id,
            format_elapsed(elapsed)
        ),
    };
    format!(
        "{header}\n  For last 50 lines:  clud tool info {tool_id}\n  For full log:       clud tool log  {tool_id}\n",
    )
}

/// Format a Duration like `26m 1s`, `2h 14m`, `42s`, `123ms`.
pub fn format_elapsed(d: Duration) -> String {
    let total_ms = d.as_millis();
    let total_secs = d.as_secs();
    if total_ms < 1000 {
        return format!("{total_ms}ms");
    }
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    match (h, m, s) {
        (h, m, _) if h > 0 => format!("{h}h {m}m"),
        (_, m, s) if m > 0 => format!("{m}m {s}s"),
        (_, _, s) => format!("{s}s"),
    }
}

/// Emit JSON payload + pointer block to stderr in one go. Convenience
/// for `tool_run.rs` which always calls them together.
#[allow(clippy::too_many_arguments)]
pub fn emit_termination(
    session_pid: u32,
    tool_id: u32,
    tool: &str,
    args: &[String],
    started_at_ms: u128,
    ended_at_ms: u128,
    elapsed: Duration,
    exit_kind: ExitKind,
    exit_code: Option<i32>,
) -> io::Result<()> {
    let payload = render_structured_payload(
        session_pid,
        tool_id,
        tool,
        args,
        started_at_ms,
        ended_at_ms,
        elapsed,
        exit_kind,
        exit_code,
    );
    let block = render_pointer_block(tool_id, tool, elapsed, exit_kind);
    let mut err = io::stderr().lock();
    writeln!(err, "{payload}")?;
    err.write_all(block.as_bytes())?;
    err.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload_value(
        session_pid: u32,
        tool_id: u32,
        exit_kind: ExitKind,
        exit_code: Option<i32>,
    ) -> serde_json::Value {
        let s = render_structured_payload(
            session_pid,
            tool_id,
            "docker-build",
            &["arg1".to_string()],
            1_000,
            2_000,
            Duration::from_millis(1000),
            exit_kind,
            exit_code,
        );
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn finished_payload_has_no_reason() {
        let v = payload_value(47180, 3, ExitKind::Finished, Some(0));
        assert_eq!(v["v"], 1);
        assert_eq!(v["tool_id"], 3);
        assert_eq!(v["long_id"], "47180-3");
        assert_eq!(v["status"], "finished");
        assert!(v["reason"].is_null());
        assert_eq!(v["exit_code"], 0);
        assert_eq!(v["elapsed_ms"], 1000);
    }

    #[test]
    fn failed_payload_carries_exit_code() {
        let v = payload_value(47180, 3, ExitKind::Failed, Some(2));
        assert_eq!(v["status"], "failed");
        assert_eq!(v["exit_code"], 2);
    }

    #[test]
    fn aborted_payload_carries_reason() {
        let v = payload_value(
            47180,
            3,
            ExitKind::Aborted(AbortReason::CommandTimeout),
            None,
        );
        assert_eq!(v["status"], "aborted");
        assert_eq!(v["reason"], "command_timeout");
        assert!(v["exit_code"].is_null());
    }

    #[test]
    fn in_progress_payload_carries_reason() {
        let v = payload_value(
            47180,
            3,
            ExitKind::InProgress(AbortReason::ProgressTimeout),
            None,
        );
        assert_eq!(v["status"], "in-progress");
        assert_eq!(v["reason"], "progress_timeout");
    }

    #[test]
    fn pointer_block_uses_session_local_integer() {
        let block = render_pointer_block(
            3,
            "docker-build",
            Duration::from_secs(60),
            ExitKind::Aborted(AbortReason::CommandTimeout),
        );
        assert!(block.contains("clud tool info 3"));
        assert!(block.contains("clud tool log  3"));
        assert!(
            !block.contains("47180-3"),
            "long-form ID should not appear in the human block"
        );
    }

    #[test]
    fn pointer_block_distinguishes_timeout_vs_stuck() {
        let timeout = render_pointer_block(
            3,
            "lint",
            Duration::from_secs(60),
            ExitKind::Aborted(AbortReason::CommandTimeout),
        );
        let stuck = render_pointer_block(
            3,
            "lint",
            Duration::from_secs(60),
            ExitKind::Aborted(AbortReason::ProgressTimeout),
        );
        assert!(timeout.contains("TIMEOUT"));
        assert!(stuck.contains("STUCK"));
    }

    #[test]
    fn pointer_block_marks_resumable_for_in_progress() {
        let block = render_pointer_block(
            3,
            "gh-pr-merge-wait",
            Duration::from_secs(60 * 20),
            ExitKind::InProgress(AbortReason::CommandTimeout),
        );
        assert!(block.contains("RESUMABLE"));
        assert!(block.contains("clud tool info 3"));
    }

    #[test]
    fn format_elapsed_seconds() {
        assert_eq!(format_elapsed(Duration::from_secs(42)), "42s");
    }

    #[test]
    fn format_elapsed_minutes_and_seconds() {
        assert_eq!(format_elapsed(Duration::from_secs(60 * 26 + 1)), "26m 1s");
    }

    #[test]
    fn format_elapsed_hours_and_minutes() {
        assert_eq!(
            format_elapsed(Duration::from_secs(60 * 60 * 2 + 14 * 60)),
            "2h 14m"
        );
    }

    #[test]
    fn format_elapsed_subsecond() {
        assert_eq!(format_elapsed(Duration::from_millis(123)), "123ms");
    }

    #[test]
    fn exit_kind_is_normal_only_for_finished() {
        assert!(ExitKind::Finished.is_normal());
        assert!(!ExitKind::Failed.is_normal());
        assert!(!ExitKind::Aborted(AbortReason::CommandTimeout).is_normal());
        assert!(!ExitKind::InProgress(AbortReason::CommandTimeout).is_normal());
    }

    #[test]
    fn payload_schema_is_v1() {
        let v = payload_value(47180, 3, ExitKind::Finished, Some(0));
        assert_eq!(
            v["v"], 1,
            "schema version is locked at 1 — change intentionally"
        );
    }
}
