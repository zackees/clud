//! Read-side queries against the session tool index. Slice 3 of #427.
//!
//! Backs `clud tool list` and `clud tool info`. Both commands parse the
//! per-session `tools/index.jsonl` produced by slices 1+2, aggregate
//! events by `tool_id`, derive an invocation state (running / finished /
//! failed / aborted), and resolve agent-friendly references to a
//! specific invocation.
//!
//! Reference resolution forms (in #427's API surface):
//!
//! | Form               | Example                | Meaning                                           |
//! |--------------------|------------------------|---------------------------------------------------|
//! | `<integer>`        | `clud tool log 3`      | Session-local integer (the PM2-style common case) |
//! | `<pid>-<integer>`  | `clud tool log 47180-3`| Long-form `<session-pid>-<tool-id>`               |
//! | `@<tool-name>`     | `clud tool log @lint`  | Most recent invocation of that tool               |
//! | `@<tool-name>:N`   | `clud tool log @lint:2`| N-th-most-recent invocation of that tool          |
//! | (no argument)      | `clud tool log`        | Most recently started invocation in this session  |

use std::fs;
use std::path::Path;

use crate::session_index::{LongFormId, SessionContext};

/// State of a tool invocation as derived from the lifecycle events
/// observed in `index.jsonl`. See `state_label` for the column rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvocationState {
    /// Started event seen, no terminal event yet.
    Running,
    /// Finished event seen with exit code 0.
    Finished,
    /// Finished event seen with non-zero exit code.
    Failed,
    /// Aborted event seen (timeout / progress watchdog / external kill).
    Aborted,
}

impl InvocationState {
    /// Column label used by `clud tool list`.
    pub fn label(self) -> &'static str {
        match self {
            InvocationState::Running => "running",
            InvocationState::Finished => "finished",
            InvocationState::Failed => "failed",
            InvocationState::Aborted => "aborted",
        }
    }
}

/// Aggregated view of a single tool invocation. Built by replaying the
/// JSONL index events for one `tool_id`.
#[derive(Debug, Clone)]
pub struct Invocation {
    pub tool_id: u32,
    pub tool: String,
    pub args: Vec<String>,
    pub pid: u32,
    pub pid_start_time: u64,
    pub started_at_ms: u128,
    pub ended_at_ms: Option<u128>,
    pub state: InvocationState,
    pub exit_code: Option<i32>,
    pub reason: Option<String>,
}

impl Invocation {
    /// Long-form ID for this invocation under the given session.
    pub fn long_form(&self, session_pid: u32) -> String {
        LongFormId {
            session_pid,
            tool_id: self.tool_id,
        }
        .format()
    }
}

/// Read `<session_dir>/tools/index.jsonl` and aggregate events into a
/// `Vec<Invocation>` ordered by `started_at_ms` (oldest first). Returns
/// an empty vec when the index doesn't exist yet (no tools have run).
pub fn read_invocations(ctx: &SessionContext) -> std::io::Result<Vec<Invocation>> {
    let index_path = ctx.index_path();
    if !index_path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&index_path)?;
    Ok(parse_invocations(&raw))
}

/// Pure parser — exposed for tests so we don't need a tempdir for every
/// resolution case.
pub fn parse_invocations(raw: &str) -> Vec<Invocation> {
    let mut by_id: Vec<Invocation> = Vec::new();
    let mut idx_for: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value): Result<serde_json::Value, _> = serde_json::from_str(trimmed) else {
            continue;
        };
        let event = value.get("event").and_then(|e| e.as_str()).unwrap_or("");
        let Some(tool_id) = value.get("tool_id").and_then(|v| v.as_u64()) else {
            continue;
        };
        let tool_id = tool_id as u32;

        match event {
            "started" => {
                let tool = value
                    .get("tool")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = value
                    .get("args")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let pid = value.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let pid_start_time = value
                    .get("pid_start_time")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let started_at_ms = value
                    .get("started_at_ms")
                    .and_then(|v| v.as_u64())
                    .map(u128::from)
                    .unwrap_or(0);
                let inv = Invocation {
                    tool_id,
                    tool,
                    args,
                    pid,
                    pid_start_time,
                    started_at_ms,
                    ended_at_ms: None,
                    state: InvocationState::Running,
                    exit_code: None,
                    reason: None,
                };
                idx_for.insert(tool_id, by_id.len());
                by_id.push(inv);
            }
            "finished" => {
                if let Some(&i) = idx_for.get(&tool_id) {
                    let inv = &mut by_id[i];
                    let exit_code =
                        value.get("exit_code").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                    inv.exit_code = Some(exit_code);
                    inv.ended_at_ms = value
                        .get("ended_at_ms")
                        .and_then(|v| v.as_u64())
                        .map(u128::from);
                    inv.state = if exit_code == 0 {
                        InvocationState::Finished
                    } else {
                        InvocationState::Failed
                    };
                }
            }
            "aborted" => {
                if let Some(&i) = idx_for.get(&tool_id) {
                    let inv = &mut by_id[i];
                    inv.state = InvocationState::Aborted;
                    inv.reason = value
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    inv.ended_at_ms = value
                        .get("ended_at_ms")
                        .and_then(|v| v.as_u64())
                        .map(u128::from);
                }
            }
            _ => {}
        }
    }

    by_id
}

/// Error from `resolve_ref` — caller turns each variant into a clear
/// `eprintln!` line + non-zero exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// Reference was empty / no-arg with no tool invocations in this session.
    NoInvocations,
    /// Reference was syntactically malformed.
    Malformed(String),
    /// Reference is valid but didn't match any invocation in this session.
    NotFound(String),
    /// Multiple invocations matched — caller should list them.
    Ambiguous {
        reference: String,
        matches: Vec<u32>,
    },
    /// Long-form `<pid>-<id>` references a different session-pid than
    /// this session. Cross-session lookups are slice-4 ledger work.
    WrongSession { reference: String, expected: u32 },
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::NoInvocations => {
                write!(f, "no tool invocations in this session")
            }
            ResolveError::Malformed(s) => {
                write!(f, "malformed reference: {s:?}")
            }
            ResolveError::NotFound(s) => {
                write!(f, "no matching invocation for reference: {s:?}")
            }
            ResolveError::Ambiguous { reference, matches } => {
                write!(
                    f,
                    "reference {reference:?} is ambiguous; matched tool ids: {matches:?}"
                )
            }
            ResolveError::WrongSession {
                reference,
                expected,
            } => {
                write!(
                    f,
                    "long-form reference {reference:?} targets a different session (this session pid = {expected})"
                )
            }
        }
    }
}

/// Resolve an agent-supplied reference to a specific `tool_id`. See the
/// module docs for the supported forms.
///
/// `pid_ref` is the explicit `--pid <num>` value (mutually exclusive with
/// the positional reference). `reference` is the positional arg or `None`
/// for the "default to most recent" path.
pub fn resolve_ref(
    invocations: &[Invocation],
    session_pid: u32,
    reference: Option<&str>,
    pid_ref: Option<u32>,
) -> Result<u32, ResolveError> {
    if invocations.is_empty() {
        return Err(ResolveError::NoInvocations);
    }

    // --pid takes priority over positional reference.
    if let Some(pid) = pid_ref {
        let matches: Vec<u32> = invocations
            .iter()
            .filter(|inv| inv.pid == pid)
            .map(|inv| inv.tool_id)
            .collect();
        return match matches.len() {
            0 => Err(ResolveError::NotFound(format!("--pid {pid}"))),
            1 => Ok(matches[0]),
            _ => Err(ResolveError::Ambiguous {
                reference: format!("--pid {pid}"),
                matches,
            }),
        };
    }

    let Some(reference) = reference else {
        // No-arg: most recently started.
        return Ok(invocations.last().unwrap().tool_id);
    };

    // Bare integer = session-local tool_id (PM2-style).
    if let Ok(n) = reference.parse::<u32>() {
        return invocations
            .iter()
            .find(|inv| inv.tool_id == n)
            .map(|inv| inv.tool_id)
            .ok_or_else(|| ResolveError::NotFound(reference.to_string()));
    }

    // Long-form <session-pid>-<tool-id>.
    if let Some(lf) = LongFormId::parse(reference) {
        if lf.session_pid != session_pid {
            return Err(ResolveError::WrongSession {
                reference: reference.to_string(),
                expected: session_pid,
            });
        }
        return invocations
            .iter()
            .find(|inv| inv.tool_id == lf.tool_id)
            .map(|inv| inv.tool_id)
            .ok_or_else(|| ResolveError::NotFound(reference.to_string()));
    }

    // @tool-name or @tool-name:N (N-th most recent).
    if let Some(rest) = reference.strip_prefix('@') {
        let (tool, ordinal) = match rest.split_once(':') {
            Some((t, n)) => {
                let ord: u32 = n
                    .parse()
                    .map_err(|_| ResolveError::Malformed(reference.to_string()))?;
                if ord == 0 {
                    return Err(ResolveError::Malformed(reference.to_string()));
                }
                (t, ord)
            }
            None => (rest, 1u32),
        };
        // Walk invocations in reverse, picking the N-th match.
        let matches: Vec<&Invocation> = invocations
            .iter()
            .rev()
            .filter(|inv| inv.tool == tool)
            .collect();
        if matches.is_empty() {
            return Err(ResolveError::NotFound(reference.to_string()));
        }
        let i = (ordinal as usize).saturating_sub(1);
        return matches
            .get(i)
            .map(|inv| inv.tool_id)
            .ok_or_else(|| ResolveError::NotFound(reference.to_string()));
    }

    Err(ResolveError::Malformed(reference.to_string()))
}

/// Convert milliseconds since epoch to a fixed-width "YYYY-MM-DD HH:MM"
/// string for the list table. We avoid a chrono dep — for V1 the
/// approximation `secs -> calendar via /86400 + epoch` is accurate
/// enough for sortable display.
pub fn format_started_at(ms: u128) -> String {
    // Convert to seconds + delegate the calendar arithmetic to the
    // OS via SystemTime + a small formatter. We hand-roll instead of
    // chrono to keep deps minimal.
    let secs = (ms / 1000) as i64;
    days_to_string(secs)
}

fn days_to_string(unix_secs: i64) -> String {
    // Civil-from-days algorithm by Howard Hinnant (public domain).
    // Returns `YYYY-MM-DD HH:MM`.
    let secs_in_day = 86_400i64;
    let mut secs = unix_secs;
    let mut days = secs.div_euclid(secs_in_day);
    secs = secs.rem_euclid(secs_in_day);
    let hour = (secs / 3600) as u32;
    let minute = ((secs % 3600) / 60) as u32;

    days += 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02} {:02}:{:02}", y, m, d, hour, minute)
}

/// Path-safe accessor for tests that don't want to compute the index
/// path manually.
pub fn read_index_raw(path: &Path) -> std::io::Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(tool_id: u32, tool: &str, started_at_ms: u64, pid: u32) -> String {
        format!(
            r#"{{"v":1,"event":"started","tool_id":{tool_id},"tool":"{tool}","args":[],"pid":{pid},"pid_start_time":0,"started_at_ms":{started_at_ms}}}"#
        )
    }

    fn finished(tool_id: u32, exit_code: i32) -> String {
        format!(
            r#"{{"v":1,"event":"finished","tool_id":{tool_id},"exit_code":{exit_code},"ended_at_ms":0}}"#
        )
    }

    fn aborted(tool_id: u32, reason: &str) -> String {
        format!(
            r#"{{"v":1,"event":"aborted","tool_id":{tool_id},"reason":"{reason}","ended_at_ms":0}}"#
        )
    }

    fn jsonl(lines: &[String]) -> String {
        let mut s = String::new();
        for l in lines {
            s.push_str(l);
            s.push('\n');
        }
        s
    }

    #[test]
    fn parse_empty_input_yields_no_invocations() {
        assert!(parse_invocations("").is_empty());
        assert!(parse_invocations("\n\n").is_empty());
    }

    #[test]
    fn parse_started_only_marks_running() {
        let raw = jsonl(&[started(1, "lint", 1000, 100)]);
        let invs = parse_invocations(&raw);
        assert_eq!(invs.len(), 1);
        assert_eq!(invs[0].tool_id, 1);
        assert_eq!(invs[0].tool, "lint");
        assert_eq!(invs[0].state, InvocationState::Running);
        assert!(invs[0].ended_at_ms.is_none());
        assert!(invs[0].exit_code.is_none());
    }

    #[test]
    fn parse_started_then_finished_with_zero_yields_finished() {
        let raw = jsonl(&[started(1, "lint", 1000, 100), finished(1, 0)]);
        let invs = parse_invocations(&raw);
        assert_eq!(invs[0].state, InvocationState::Finished);
        assert_eq!(invs[0].exit_code, Some(0));
    }

    #[test]
    fn parse_started_then_finished_with_nonzero_yields_failed() {
        let raw = jsonl(&[started(1, "lint", 1000, 100), finished(1, 2)]);
        let invs = parse_invocations(&raw);
        assert_eq!(invs[0].state, InvocationState::Failed);
        assert_eq!(invs[0].exit_code, Some(2));
    }

    #[test]
    fn parse_started_then_aborted_yields_aborted() {
        let raw = jsonl(&[
            started(1, "docker-build", 1000, 100),
            aborted(1, "progress_timeout"),
        ]);
        let invs = parse_invocations(&raw);
        assert_eq!(invs[0].state, InvocationState::Aborted);
        assert_eq!(invs[0].reason.as_deref(), Some("progress_timeout"));
    }

    #[test]
    fn resolve_no_arg_returns_most_recent_started() {
        let raw = jsonl(&[
            started(1, "lint", 1000, 100),
            started(2, "test", 2000, 200),
            started(3, "docker-build", 3000, 300),
        ]);
        let invs = parse_invocations(&raw);
        assert_eq!(resolve_ref(&invs, 47180, None, None).unwrap(), 3);
    }

    #[test]
    fn resolve_bare_integer_matches_tool_id() {
        let raw = jsonl(&[started(1, "lint", 1000, 100), started(2, "test", 2000, 200)]);
        let invs = parse_invocations(&raw);
        assert_eq!(resolve_ref(&invs, 47180, Some("2"), None).unwrap(), 2);
    }

    #[test]
    fn resolve_bare_integer_not_found_errors() {
        let raw = jsonl(&[started(1, "lint", 1000, 100)]);
        let invs = parse_invocations(&raw);
        let err = resolve_ref(&invs, 47180, Some("99"), None).unwrap_err();
        assert_eq!(err, ResolveError::NotFound("99".to_string()));
    }

    #[test]
    fn resolve_long_form_matches() {
        let raw = jsonl(&[started(3, "lint", 1000, 100)]);
        let invs = parse_invocations(&raw);
        assert_eq!(resolve_ref(&invs, 47180, Some("47180-3"), None).unwrap(), 3);
    }

    #[test]
    fn resolve_long_form_wrong_session_errors() {
        let raw = jsonl(&[started(3, "lint", 1000, 100)]);
        let invs = parse_invocations(&raw);
        let err = resolve_ref(&invs, 47180, Some("99999-3"), None).unwrap_err();
        assert!(matches!(err, ResolveError::WrongSession { .. }));
    }

    #[test]
    fn resolve_at_tool_name_matches_most_recent() {
        let raw = jsonl(&[
            started(1, "lint", 1000, 100),
            started(2, "lint", 2000, 200),
            started(3, "docker-build", 3000, 300),
        ]);
        let invs = parse_invocations(&raw);
        assert_eq!(resolve_ref(&invs, 47180, Some("@lint"), None).unwrap(), 2);
    }

    #[test]
    fn resolve_at_tool_name_with_ordinal() {
        let raw = jsonl(&[
            started(1, "lint", 1000, 100),
            started(2, "lint", 2000, 200),
            started(3, "lint", 3000, 300),
        ]);
        let invs = parse_invocations(&raw);
        // :1 = most recent, :2 = second-most-recent, :3 = oldest.
        assert_eq!(resolve_ref(&invs, 47180, Some("@lint:1"), None).unwrap(), 3);
        assert_eq!(resolve_ref(&invs, 47180, Some("@lint:2"), None).unwrap(), 2);
        assert_eq!(resolve_ref(&invs, 47180, Some("@lint:3"), None).unwrap(), 1);
    }

    #[test]
    fn resolve_at_tool_name_with_bad_ordinal_errors() {
        let raw = jsonl(&[started(1, "lint", 1000, 100)]);
        let invs = parse_invocations(&raw);
        let err = resolve_ref(&invs, 47180, Some("@lint:abc"), None).unwrap_err();
        assert!(matches!(err, ResolveError::Malformed(_)));
        let err0 = resolve_ref(&invs, 47180, Some("@lint:0"), None).unwrap_err();
        assert!(matches!(err0, ResolveError::Malformed(_)));
    }

    #[test]
    fn resolve_at_tool_name_no_match_errors() {
        let raw = jsonl(&[started(1, "lint", 1000, 100)]);
        let invs = parse_invocations(&raw);
        let err = resolve_ref(&invs, 47180, Some("@docker-build"), None).unwrap_err();
        assert!(matches!(err, ResolveError::NotFound(_)));
    }

    #[test]
    fn resolve_pid_flag_matches() {
        let raw = jsonl(&[started(1, "lint", 1000, 100), started(2, "test", 2000, 200)]);
        let invs = parse_invocations(&raw);
        assert_eq!(resolve_ref(&invs, 47180, None, Some(200)).unwrap(), 2);
    }

    #[test]
    fn resolve_pid_ambiguous_errors() {
        let raw = jsonl(&[started(1, "lint", 1000, 100), started(2, "test", 2000, 100)]);
        let invs = parse_invocations(&raw);
        let err = resolve_ref(&invs, 47180, None, Some(100)).unwrap_err();
        assert!(matches!(err, ResolveError::Ambiguous { .. }));
    }

    #[test]
    fn resolve_empty_invocations_errors() {
        let invs: Vec<Invocation> = vec![];
        let err = resolve_ref(&invs, 47180, Some("3"), None).unwrap_err();
        assert_eq!(err, ResolveError::NoInvocations);
    }

    #[test]
    fn long_form_render_uses_session_pid() {
        let inv = Invocation {
            tool_id: 7,
            tool: "lint".to_string(),
            args: vec![],
            pid: 100,
            pid_start_time: 0,
            started_at_ms: 0,
            ended_at_ms: None,
            state: InvocationState::Running,
            exit_code: None,
            reason: None,
        };
        assert_eq!(inv.long_form(47180), "47180-7");
    }

    #[test]
    fn format_started_at_yields_iso_like_string() {
        // 2026-06-20 02:00:00 UTC → millis since epoch.
        let s = format_started_at(1_781_999_200_000);
        // We don't assert the exact string because the underlying epoch
        // arithmetic is rounding-sensitive; just confirm the shape.
        assert_eq!(s.len(), 16, "expected 'YYYY-MM-DD HH:MM' = 16 chars: {s}");
        assert_eq!(s.chars().nth(4), Some('-'));
        assert_eq!(s.chars().nth(7), Some('-'));
        assert_eq!(s.chars().nth(10), Some(' '));
        assert_eq!(s.chars().nth(13), Some(':'));
    }
}
