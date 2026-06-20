//! `clud tool log <ref> [filters]` — slice 4 of #427.
//!
//! Reads the per-invocation JSONL log from the tee writer (slice 2) and
//! prints filtered output. Filters: `--since`/`--until`/`--between` for
//! time ranges, `--grep` for substring matching on decoded lines,
//! `--stream` to pick stdout/stderr/combined, `--head`/`--tail` for
//! quantity. `--json` returns the raw JSONL entries instead of decoding
//! them into human-readable text.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;

use crate::session_index::SessionContext;
use crate::tool_query::{read_invocations, resolve_ref};

/// Which JSONL file to read. Mirrors the `--stream` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamSelector {
    Stdout,
    Stderr,
    Combined,
}

impl StreamSelector {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "stdout" => Some(Self::Stdout),
            "stderr" => Some(Self::Stderr),
            "combined" | "all" => Some(Self::Combined),
            _ => None,
        }
    }

    pub fn filename(self) -> &'static str {
        match self {
            Self::Stdout => "stdout.jsonl",
            Self::Stderr => "stderr.jsonl",
            Self::Combined => "combined.jsonl",
        }
    }
}

/// Filters applied to each parsed log entry before emission.
#[derive(Debug, Clone, Default)]
pub struct LogFilters {
    pub since_ms: Option<u128>,
    pub until_ms: Option<u128>,
    pub grep: Option<String>,
    pub head: Option<usize>,
    pub tail: Option<usize>,
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    reference: Option<&str>,
    pid: Option<u32>,
    stream: StreamSelector,
    since: Option<&str>,
    until: Option<&str>,
    between: Option<(&str, &str)>,
    grep: Option<&str>,
    head: Option<usize>,
    tail: Option<usize>,
    json: bool,
) -> io::Result<i32> {
    let Some(ctx) = SessionContext::from_env() else {
        eprintln!("[clud] tool log: no clud session active (CLUD_SESSION_PID unset)");
        return Ok(2);
    };
    let invocations = read_invocations(&ctx)?;
    let tool_id = match resolve_ref(&invocations, ctx.session_pid, reference, pid) {
        Ok(id) => id,
        Err(err) => {
            eprintln!("[clud] tool log: {err}");
            return Ok(2);
        }
    };

    // Compute the absolute time window once. `--between` overrides
    // `--since`/`--until` if all three are supplied (clap should
    // prevent that combination but be conservative).
    let now_ms = crate::session_index::unix_millis_now();
    let mut since_ms = since
        .and_then(parse_duration_secs)
        .map(|d| now_ms.saturating_sub((d.as_secs() as u128) * 1000));
    let mut until_ms = until
        .and_then(parse_duration_secs)
        .map(|d| now_ms.saturating_sub((d.as_secs() as u128) * 1000));
    if let Some((start, end)) = between {
        if let Some(s) = parse_epoch_or_rfc3339(start) {
            since_ms = Some(s);
        }
        if let Some(e) = parse_epoch_or_rfc3339(end) {
            until_ms = Some(e);
        }
    }

    let filters = LogFilters {
        since_ms,
        until_ms,
        grep: grep.map(str::to_string),
        head,
        tail,
    };

    let log_path = ctx.tool_log_dir(tool_id).join(stream.filename());
    emit_log(&log_path, &filters, json)
}

fn emit_log(path: &Path, filters: &LogFilters, json: bool) -> io::Result<i32> {
    if !path.exists() {
        eprintln!("[clud] tool log: no log file at {}", path.display());
        return Ok(2);
    }
    let raw = fs::read_to_string(path)?;
    let mut matched: Vec<serde_json::Value> = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value): Result<serde_json::Value, _> = serde_json::from_str(trimmed) else {
            continue;
        };
        let ts_ms = value.get("ts_ms").and_then(|v| v.as_u64()).map(u128::from);
        if let (Some(ts), Some(since)) = (ts_ms, filters.since_ms) {
            if ts < since {
                continue;
            }
        }
        if let (Some(ts), Some(until)) = (ts_ms, filters.until_ms) {
            if ts > until {
                continue;
            }
        }
        if let Some(pattern) = filters.grep.as_deref() {
            // Decode bytes to a lossy string and match.
            let decoded = decode_bytes(&value);
            let s = String::from_utf8_lossy(&decoded);
            if !s.contains(pattern) {
                continue;
            }
        }
        matched.push(value);
    }
    // Apply head/tail.
    let total = matched.len();
    let view: Vec<&serde_json::Value> = match (filters.head, filters.tail) {
        (Some(n), _) => matched.iter().take(n).collect(),
        (None, Some(n)) => {
            let start = total.saturating_sub(n);
            matched[start..].iter().collect()
        }
        _ => matched.iter().collect(),
    };

    let mut out = io::stdout().lock();
    if json {
        for v in view {
            serde_json::to_writer(&mut out, v)?;
            out.write_all(b"\n")?;
        }
    } else {
        for v in view {
            let decoded = decode_bytes(v);
            // Strip trailing newline to avoid double-newlines.
            out.write_all(&decoded)?;
        }
        out.flush()?;
    }
    Ok(0)
}

fn decode_bytes(value: &serde_json::Value) -> Vec<u8> {
    value
        .get("bytes")
        .and_then(|v| v.as_str())
        .and_then(|b| STANDARD_NO_PAD.decode(b).ok())
        .unwrap_or_default()
}

/// Parse a duration like `5m`, `1h`, `30s`, `2d`. Returns `None` for
/// anything unrecognized.
pub fn parse_duration_secs(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num_part, unit) = s.split_at(s.len() - 1);
    let n: u64 = num_part.parse().ok()?;
    let multiplier = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 60 * 60 * 24,
        _ => return None,
    };
    Some(Duration::from_secs(n.checked_mul(multiplier)?))
}

/// Parse either an integer epoch-ms or an `rfc3339`-ish prefix. V1 only
/// accepts the integer form; users can compute it from their preferred
/// timezone via `date +%s%3N`. Future versions could accept full RFC3339
/// without a dep.
pub fn parse_epoch_or_rfc3339(s: &str) -> Option<u128> {
    s.trim().parse::<u128>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_seconds() {
        assert_eq!(parse_duration_secs("30s"), Some(Duration::from_secs(30)));
    }

    #[test]
    fn parse_duration_minutes() {
        assert_eq!(parse_duration_secs("5m"), Some(Duration::from_secs(300)));
    }

    #[test]
    fn parse_duration_hours() {
        assert_eq!(parse_duration_secs("2h"), Some(Duration::from_secs(7200)));
    }

    #[test]
    fn parse_duration_days() {
        assert_eq!(parse_duration_secs("1d"), Some(Duration::from_secs(86400)));
    }

    #[test]
    fn parse_duration_rejects_garbage() {
        assert_eq!(parse_duration_secs(""), None);
        assert_eq!(parse_duration_secs("abc"), None);
        assert_eq!(parse_duration_secs("5x"), None);
        assert_eq!(parse_duration_secs("m"), None);
    }

    #[test]
    fn stream_selector_from_str_recognizes_each_kind() {
        assert_eq!(
            StreamSelector::parse("stdout"),
            Some(StreamSelector::Stdout)
        );
        assert_eq!(
            StreamSelector::parse("stderr"),
            Some(StreamSelector::Stderr)
        );
        assert_eq!(
            StreamSelector::parse("combined"),
            Some(StreamSelector::Combined)
        );
        assert_eq!(StreamSelector::parse("all"), Some(StreamSelector::Combined));
        assert_eq!(StreamSelector::parse("garbage"), None);
    }

    #[test]
    fn parse_epoch_accepts_integer() {
        assert_eq!(
            parse_epoch_or_rfc3339("1700000000000"),
            Some(1_700_000_000_000u128)
        );
        assert_eq!(parse_epoch_or_rfc3339("abc"), None);
    }
}
