//! Forensic session state (`~/.clud/state/sessions/`) — issue #1014.
//!
//! Every launch leaves a `<pid>__<start-epoch>/` directory here holding the
//! reaper's `reap.jsonl` / `reap-health.json` and, since #1011, a
//! `bridge.jsonl`. Nothing aged that tree out, so it grew one entry per
//! session forever. The sibling `state/launches/` tree has been bounded since
//! #998 (`MAX_RECORDS` in `launch_log.rs`); this is the equivalent for
//! sessions.
//!
//! The entries are tiny, so this is not about disk. It is about a directory
//! that eventually holds one child per session you have ever run, which costs
//! listing time and makes the tree unsearchable by hand during an incident —
//! exactly when someone needs it.
//!
//! Two invariants the issue calls out, and this module exists to honor:
//!
//! 1. **A live session's directory is never touched.** The reaper and the
//!    bridge both write into it for the whole life of the launch. Liveness is
//!    injected as a predicate rather than called directly, both so the daemon
//!    keeps ownership of `pid_is_alive` and so the decision table is testable
//!    without spawning processes.
//! 2. **Failures outlive successes.** The point of #998 and #1011 is that the
//!    forensic trail survives to be read afterwards. A `bridge.jsonl` holding
//!    only ambient chatter is worth keeping while it is fresh and worth
//!    nothing once it is old; one holding a refusal or an upstream failure is
//!    the thing someone comes looking for weeks later.
//!
//! Errors are non-fatal throughout: a missed sweep never crashes the daemon.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use super::delete_audit;

/// Age after which a session directory with nothing notable in it goes. Shares
/// 48h with the session-temp and worktree policies, deliberately as its own
/// constant — they are independent policies that agree today.
pub const AMBIENT_STALE_AFTER: Duration = Duration::from_secs(48 * 60 * 60);

/// Age after which even a directory holding a failure goes. Long enough that
/// "it broke sometime last month" is still answerable, bounded so the tree
/// cannot grow without limit for a user who hits frequent failures.
pub const NOTABLE_STALE_AFTER: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Bridge events written via `BridgeLog::record_ambient` — context that is
/// useful *beside* a failure but is not itself a reason to look (#999, #1011).
///
/// The on-disk JSONL does not mark notability per line, so the set is
/// mirrored here. It must track the `record_ambient` call sites in
/// `codex_bridge.rs`; `ambient_event_names_match_record_ambient_call_sites`
/// in the daemon-side tests is the reminder.
///
/// Drift fails *safe*: an ambient event missing from this list reads as
/// notable, and the directory is retained longer than it needed to be. The
/// opposite mistake — silently discarding a failure trail — is the one that
/// matters, and this ordering makes it impossible.
const AMBIENT_EVENTS: &[&str] = &[
    "admission_acquired",
    "admission_queued",
    "catalog_advertised",
];

/// Read-only view of [`AMBIENT_EVENTS`], so the daemon-side drift test can
/// compare it against `codex_bridge.rs`'s `record_ambient` call sites.
pub fn ambient_events() -> &'static [&'static str] {
    AMBIENT_EVENTS
}

/// The bridge log inside a session directory.
const BRIDGE_LOG: &str = "bridge.jsonl";

/// Outcome of one sweep. The `kept_*` counters are split so the log line can
/// say *why* a tree did not shrink, which is the question an operator asks
/// when they expected it to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SweepReport {
    pub removed: usize,
    pub kept_live: usize,
    pub kept_notable: usize,
    pub skipped: usize,
}

/// What a single directory's contents say about how long to keep it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    /// Nothing but ambient records (or no bridge log at all).
    Ambient,
    /// Holds something an operator would come looking for.
    Notable,
}

impl Retention {
    pub fn stale_after(self) -> Duration {
        match self {
            Retention::Ambient => AMBIENT_STALE_AFTER,
            Retention::Notable => NOTABLE_STALE_AFTER,
        }
    }
}

/// Parse the `<pid>__<start-epoch>` directory name written by
/// `SessionContext`. `None` for anything else — a foreign entry is left alone
/// rather than guessed at.
pub fn parse_session_dir_name(name: &str) -> Option<(u32, u64)> {
    let (pid, epoch) = name.split_once("__")?;
    Some((pid.parse().ok()?, epoch.parse().ok()?))
}

/// Classify a session directory by what its bridge log holds.
///
/// A log that cannot be read or whose lines do not parse counts as notable:
/// an unreadable forensic trail is the last thing to delete on a guess.
pub fn classify(dir: &Path) -> Retention {
    let path = dir.join(BRIDGE_LOG);
    if !path.is_file() {
        // No bridge log at all — reaper-only session, nothing to preserve
        // beyond the ambient window.
        return Retention::Ambient;
    }
    let Ok(body) = fs::read_to_string(&path) else {
        return Retention::Notable;
    };
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return Retention::Notable;
        };
        match value.get("event").and_then(serde_json::Value::as_str) {
            Some(event) if AMBIENT_EVENTS.contains(&event) => {}
            // A named non-ambient event, or a record with no `event` field at
            // all — either way, not something to discard early.
            _ => return Retention::Notable,
        }
    }
    Retention::Ambient
}

/// Sweep `root`, dropping session directories past their retention window.
///
/// `is_alive` decides invariant 1. A recycled PID can therefore keep one small
/// directory alive until that unrelated process exits; that is the safe
/// direction and costs a few hundred bytes.
pub fn sweep_at<F>(root: &Path, now: SystemTime, is_alive: F) -> std::io::Result<SweepReport>
where
    F: Fn(u32) -> bool,
{
    let mut report = SweepReport::default();
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        // No tree yet is a successful no-op, not an error.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(err) => return Err(err),
    };

    for entry in entries {
        let Ok(entry) = entry else {
            report.skipped += 1;
            continue;
        };
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some((pid, _epoch)) = parse_session_dir_name(name) else {
            // Not one of ours.
            continue;
        };
        if is_alive(pid) {
            report.kept_live += 1;
            continue;
        }
        let Ok(age) = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .map(|modified| now.duration_since(modified).unwrap_or_default())
        else {
            report.skipped += 1;
            continue;
        };
        let retention = classify(&path);
        if age <= retention.stale_after() {
            if retention == Retention::Notable {
                report.kept_notable += 1;
            }
            continue;
        }
        // Audit *before* acting (#893), the same rule every other destructive
        // sweep follows: a deletion that leaves no record is its own defect,
        // and a line written first survives a crash mid-deletion. These
        // directories hold the reaper and bridge forensics someone may come
        // looking for, so "what removed my session log?" has to be answerable.
        delete_audit::record(
            "gc.session-state",
            &path,
            match retention {
                Retention::Ambient => "session-state ambient stale>48h",
                Retention::Notable => "session-state notable stale>30d",
            },
        );
        match fs::remove_dir_all(&path) {
            Ok(()) => report.removed += 1,
            // Losing the race with a starting session, or a permission
            // problem — the next sweep retries.
            Err(_) => report.skipped += 1,
        }
    }
    Ok(report)
}

#[cfg(test)]
#[path = "session_state_tests.rs"]
mod tests;
