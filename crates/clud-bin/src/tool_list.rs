//! `clud tool list` formatter. Slice 3 of #427.
//!
//! Renders the session-scoped invocation table (most-recent first) in
//! either plain table form or JSON. The shape is documented in #427's
//! locked-in API surface.

use std::io;

use crate::session_index::SessionContext;
use crate::tool_query::{format_started_at, read_invocations, Invocation};

/// Run `clud tool list`. Returns the desired process exit code.
pub fn run(json: bool, long: bool) -> io::Result<i32> {
    let Some(ctx) = SessionContext::from_env() else {
        eprintln!("[clud] tool list: no clud session active (CLUD_SESSION_PID unset)");
        return Ok(2);
    };
    let invocations = read_invocations(&ctx)?;
    let session_pid = ctx.session_pid;
    if json {
        print_json(session_pid, &invocations)?;
    } else {
        print_table(session_pid, &invocations, long);
    }
    Ok(0)
}

fn print_json(session_pid: u32, invocations: &[Invocation]) -> io::Result<()> {
    use std::io::Write;
    let entries: Vec<serde_json::Value> = invocations
        .iter()
        .rev()
        .map(|inv| {
            serde_json::json!({
                "tool_id": inv.tool_id,
                "long_id": inv.long_form(session_pid),
                "pid": inv.pid,
                "started_at_ms": inv.started_at_ms as u64,
                "ended_at_ms": inv.ended_at_ms.map(|v| v as u64),
                "state": inv.state.label(),
                "tool": inv.tool,
                "args": inv.args,
                "exit_code": inv.exit_code,
                "reason": inv.reason,
            })
        })
        .collect();
    let value = serde_json::Value::Array(entries);
    let mut out = io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, &value)?;
    out.write_all(b"\n")
}

fn print_table(session_pid: u32, invocations: &[Invocation], long: bool) {
    // Header.
    let id_col = if long { "LONG-ID" } else { "ID" };
    println!(
        "{:<10} {:<7} {:<16} {:<10} {:<24} ARGS",
        id_col, "PID", "START-TIME", "STATE", "TOOL"
    );
    // Empty-state hint.
    if invocations.is_empty() {
        println!("(no tool invocations in this session)");
        return;
    }
    // Body — most recent first.
    for inv in invocations.iter().rev() {
        let id_cell = if long {
            inv.long_form(session_pid)
        } else {
            inv.tool_id.to_string()
        };
        let args = inv.args.join(" ");
        let args = if args.len() > 40 {
            format!("{}…", &args[..39])
        } else {
            args
        };
        println!(
            "{:<10} {:<7} {:<16} {:<10} {:<24} {}",
            id_cell,
            inv.pid,
            format_started_at(inv.started_at_ms),
            inv.state.label(),
            truncate(&inv.tool, 24),
            args
        );
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n.saturating_sub(1)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_input_returns_unchanged() {
        assert_eq!(truncate("lint", 24), "lint");
    }

    #[test]
    fn truncate_long_input_appends_ellipsis() {
        let s = "a".repeat(50);
        let t = truncate(&s, 10);
        assert!(t.ends_with('…'));
        assert!(t.chars().count() <= 10);
    }
}
