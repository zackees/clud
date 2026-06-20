//! `clud tool ledger [--tool X] [--session current|previous|all]` —
//! slice 4 of #427.
//!
//! History view of tool invocations across one or more sessions. The
//! current-session case overlaps with `clud tool list` but `ledger`
//! grows additional filters (`--tool`) and scopes (`--session`) without
//! bloating the `list` UX. `--session all` uses the long-form
//! `<session-pid>-<tool-id>` IDs because session-local integers would be
//! ambiguous across sessions.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::session_index::SessionContext;
use crate::tool_query::{format_started_at, parse_invocations, Invocation};

/// Scope of the ledger view. Maps to the `--session` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionScope {
    Current,
    Previous,
    All,
}

impl SessionScope {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "current" => Some(Self::Current),
            "previous" => Some(Self::Previous),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

/// One row in the ledger view, carrying enough context to render either
/// the session-local integer (single-session scope) or the long-form
/// `<session-pid>-<tool-id>` (cross-session scope).
#[derive(Debug, Clone)]
pub struct LedgerEntry {
    pub session_pid: u32,
    pub session_start_epoch: u64,
    pub inv: Invocation,
}

pub fn run(tool_filter: Option<&str>, scope: SessionScope, json: bool) -> io::Result<i32> {
    let entries = match scope {
        SessionScope::Current => collect_current()?,
        SessionScope::Previous => collect_previous()?,
        SessionScope::All => collect_all()?,
    };
    let filtered: Vec<&LedgerEntry> = entries
        .iter()
        .filter(|e| match tool_filter {
            Some(name) => e.inv.tool == name,
            None => true,
        })
        .collect();
    let cross_session = matches!(scope, SessionScope::All | SessionScope::Previous);
    if json {
        print_json(&filtered)?;
    } else {
        print_table(&filtered, cross_session);
    }
    Ok(0)
}

fn collect_current() -> io::Result<Vec<LedgerEntry>> {
    let Some(ctx) = SessionContext::from_env() else {
        return Ok(Vec::new());
    };
    load_session(&ctx.session_dir, ctx.session_pid, ctx.session_start_epoch)
}

fn collect_previous() -> io::Result<Vec<LedgerEntry>> {
    let dirs = enumerate_session_dirs()?;
    if dirs.len() < 2 {
        return Ok(Vec::new());
    }
    // enumerate_session_dirs returns newest-first; index 1 is the previous.
    let prev = &dirs[1];
    load_session(&prev.path, prev.session_pid, prev.start_epoch)
}

fn collect_all() -> io::Result<Vec<LedgerEntry>> {
    let dirs = enumerate_session_dirs()?;
    let mut all = Vec::new();
    for d in dirs {
        let entries = load_session(&d.path, d.session_pid, d.start_epoch)?;
        all.extend(entries);
    }
    // Cross-session ordering: by started_at_ms newest-first when we print.
    all.sort_by_key(|e| std::cmp::Reverse(e.inv.started_at_ms));
    Ok(all)
}

fn load_session(
    session_dir: &Path,
    session_pid: u32,
    session_start_epoch: u64,
) -> io::Result<Vec<LedgerEntry>> {
    let index_path = session_dir.join("tools").join("index.jsonl");
    if !index_path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&index_path)?;
    let invs = parse_invocations(&raw);
    Ok(invs
        .into_iter()
        .map(|inv| LedgerEntry {
            session_pid,
            session_start_epoch,
            inv,
        })
        .collect())
}

#[derive(Debug, Clone)]
struct SessionDir {
    path: PathBuf,
    session_pid: u32,
    start_epoch: u64,
}

/// Walk `~/.clud/state/sessions/` and parse each `<pid>__<epoch>`
/// subdirectory name. Returns newest-first by `<epoch>`.
fn enumerate_session_dirs() -> io::Result<Vec<SessionDir>> {
    let Some(root) = state_root() else {
        return Ok(Vec::new());
    };
    let sessions_root = root.join("sessions");
    if !sessions_root.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&sessions_root)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some((pid_str, epoch_str)) = name.split_once("__") else {
            continue;
        };
        let Ok(session_pid) = pid_str.parse::<u32>() else {
            continue;
        };
        let Ok(start_epoch) = epoch_str.parse::<u64>() else {
            continue;
        };
        out.push(SessionDir {
            path,
            session_pid,
            start_epoch,
        });
    }
    out.sort_by_key(|d| std::cmp::Reverse(d.start_epoch));
    Ok(out)
}

fn print_table(entries: &[&LedgerEntry], cross_session: bool) {
    let id_col = if cross_session { "LONG-ID" } else { "ID" };
    println!(
        "{:<14} {:<7} {:<16} {:<10} {:<24} ARGS",
        id_col, "PID", "START-TIME", "STATE", "TOOL"
    );
    if entries.is_empty() {
        println!("(no matching invocations)");
        return;
    }
    for e in entries {
        let id_cell = if cross_session {
            e.inv.long_form(e.session_pid)
        } else {
            e.inv.tool_id.to_string()
        };
        let args = e.inv.args.join(" ");
        let args = if args.len() > 40 {
            format!("{}…", &args[..39])
        } else {
            args
        };
        println!(
            "{:<14} {:<7} {:<16} {:<10} {:<24} {}",
            id_cell,
            e.inv.pid,
            format_started_at(e.inv.started_at_ms),
            e.inv.state.label(),
            truncate(&e.inv.tool, 24),
            args
        );
    }
}

fn print_json(entries: &[&LedgerEntry]) -> io::Result<()> {
    let arr: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "session_pid": e.session_pid,
                "session_start_epoch": e.session_start_epoch,
                "tool_id": e.inv.tool_id,
                "long_id": e.inv.long_form(e.session_pid),
                "tool": e.inv.tool,
                "args": e.inv.args,
                "pid": e.inv.pid,
                "state": e.inv.state.label(),
                "exit_code": e.inv.exit_code,
                "reason": e.inv.reason,
                "started_at_ms": e.inv.started_at_ms as u64,
                "ended_at_ms": e.inv.ended_at_ms.map(|v| v as u64),
            })
        })
        .collect();
    let mut out = io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, &serde_json::Value::Array(arr))?;
    out.write_all(b"\n")
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n.saturating_sub(1)])
    }
}

fn state_root() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".clud").join("state"))
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_scope_parse() {
        assert_eq!(SessionScope::parse("current"), Some(SessionScope::Current));
        assert_eq!(
            SessionScope::parse("previous"),
            Some(SessionScope::Previous)
        );
        assert_eq!(SessionScope::parse("all"), Some(SessionScope::All));
        assert_eq!(SessionScope::parse("nope"), None);
    }
}
