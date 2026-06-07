//! Cross-path Ctrl+C exit timing.
//!
//! Records the moment the process most recently observes Ctrl+C (whether
//! under the direct runner, an attached daemon session, or the centralized
//! launch path) and, just before the process exits, writes a JSON event
//! under `<state_dir>/ctrl_c_events/<unix-ms>-<pid>.json` capturing the
//! elapsed wall-clock time from "Ctrl+C seen" to "about to exit". The
//! daemon dashboard reads these files and surfaces them on `clud ui` so
//! the recurring "Ctrl+C takes forever to drop me back at the shell"
//! problem has hard numbers attached.
//!
//! Every Ctrl+C re-stamps the observation point (issue #285 rec 1): the
//! prior `OnceLock` design only stamped the very first Ctrl+C of the
//! process's lifetime, so a user who pressed Ctrl+C once to clear a
//! backend prompt, kept working, then later pressed Ctrl+C to exit, would
//! see the entire intervening session attributed to a single "slow"
//! event. The latest observation always wins.
//!
//! In addition, the teardown sites record the daemon-handoff outcome
//! (issue #285 rec 2) so the dashboard can distinguish "daemon adopted
//! the kill in <100ms" from "fell back to synchronous kill_tree" at a
//! glance.
//!
//! The on-disk format is intentionally tiny and forwards-compatible:
//! unknown fields are ignored on read, and the directory is capped so a
//! long-running daemon never accumulates more than [`MAX_RETAINED_EVENTS`]
//! files. Existing per-session [`crate::daemon::types::CtrlCProfile`]
//! handoff/kill telemetry is unchanged.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const EVENTS_DIRNAME: &str = "ctrl_c_events";

/// Hard cap on retained events. The dashboard only needs the recent tail,
/// and we don't want a debugging dir to balloon over a long-lived daemon.
pub const MAX_RETAINED_EVENTS: usize = 50;

/// Cap returned by [`read_recent_events`] so `/state.json` payloads stay
/// small even right after a burst of interrupts.
pub const DASHBOARD_EVENT_LIMIT: usize = 20;

/// Origin of the interrupt — the dashboard groups events by this so the
/// "is it the daemon attach path that's slow or the direct runner?"
/// question has a one-glance answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationKind {
    Direct,
    Attach,
    Centralized,
}

impl InvocationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Attach => "attach",
            Self::Centralized => "centralized",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtrlCEvent {
    pub pid: u32,
    pub observed_at_ms: u64,
    pub exit_at_ms: u64,
    pub elapsed_ms: u64,
    pub kind: InvocationKind,
    pub exit_code: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Whether the daemon adopted the kill on the fast path. `None`
    /// means the teardown site never recorded an outcome (older event
    /// files, or `clud --no-daemon` paths that don't run the teardown
    /// helper). The dashboard surfaces this so "daemon adopted" vs
    /// "synchronous fallback" is one-glance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handed_off: Option<bool>,
    /// Free-form tag explaining the handoff outcome
    /// (e.g. `"ctrl_c_subprocess"` on success or
    /// `"daemon_unreachable"` / `"no_state_dir"` on failure). Optional
    /// so old event files stay parseable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_reason: Option<String>,
}

// Process-wide observation point. Re-stamped on every Ctrl+C the signal
// handler sees (issue #285 rec 1), so `build_event` always measures from
// the most recent press — not the very first one. `AtomicU64::store` is
// signal-safe on every platform clud targets, so this works equally well
// from POSIX signal handlers and from the Windows console-handler thread.
//
// A zero value means "never observed" — the unix epoch is the natural
// sentinel and saves us a separate boolean flag.
static OBSERVED_UNIX_MS: AtomicU64 = AtomicU64::new(0);

/// Process-wide handoff outcome. Recorded by the teardown sites
/// (`runner::teardown_interrupted_child`, `session::interrupt_pty_process`)
/// after they decide whether the daemon adopted the kill or the legacy
/// fallback ran. Lives in a `Mutex` (not an atomic) because the reason
/// string would otherwise need bespoke encoding; teardown sites run on
/// the main thread, never inside a signal handler, so lock acquisition
/// is safe here.
#[derive(Debug, Clone)]
pub struct HandoffOutcome {
    pub handed_off: bool,
    pub reason: Option<String>,
}

static HANDOFF_OUTCOME: Mutex<Option<HandoffOutcome>> = Mutex::new(None);

/// Mark the process as having observed Ctrl+C. Safe to call from a signal
/// handler — no allocations, no locks, just an atomic store.
///
/// Unlike the prior `OnceLock`-based design, every call overwrites the
/// previous timestamp (issue #285 rec 1). This is intentional: we want
/// `elapsed_ms` to measure "the Ctrl+C that exited clud → shell return",
/// not "the very first Ctrl+C ever seen → exit", which conflated multiple
/// presses across a long session into a single bogus 5-minute event.
pub fn record_observed() {
    OBSERVED_UNIX_MS.store(unix_millis_now(), Ordering::SeqCst);
}

pub fn was_observed() -> bool {
    OBSERVED_UNIX_MS.load(Ordering::SeqCst) != 0
}

/// Record the daemon-handoff outcome (issue #285 rec 2). Called from
/// `runner::teardown_interrupted_child` / `session::interrupt_pty_process`
/// right after they consult `try_handoff_kill_to_daemon`. The last
/// outcome before exit wins, matching the observation-point semantics
/// above. Best-effort: a poisoned mutex is silently ignored so this
/// helper can never block exit.
pub fn record_handoff(handed_off: bool, reason: Option<&str>) {
    if let Ok(mut guard) = HANDOFF_OUTCOME.lock() {
        *guard = Some(HandoffOutcome {
            handed_off,
            reason: reason.map(|s| s.to_string()),
        });
    }
}

/// If Ctrl+C was observed during this process's lifetime, write an event
/// file under `<state_dir>/ctrl_c_events/`. Best-effort: every error path
/// is silent. This must never block exit.
pub fn flush_on_exit(state_dir: &Path, kind: InvocationKind, exit_code: i32) {
    let Some(event) = build_event(kind, exit_code) else {
        return;
    };
    let _ = write_event(state_dir, &event);
}

fn build_event(kind: InvocationKind, exit_code: i32) -> Option<CtrlCEvent> {
    let observed_at_ms = OBSERVED_UNIX_MS.load(Ordering::SeqCst);
    if observed_at_ms == 0 {
        return None;
    }
    let exit_at_ms = unix_millis_now();
    let elapsed_ms = exit_at_ms.saturating_sub(observed_at_ms);
    let cwd = std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    let (handed_off, handoff_reason) = HANDOFF_OUTCOME
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .map(|o| (Some(o.handed_off), o.reason))
        .unwrap_or((None, None));
    Some(CtrlCEvent {
        pid: std::process::id(),
        observed_at_ms,
        exit_at_ms,
        elapsed_ms,
        kind,
        exit_code,
        cwd,
        handed_off,
        handoff_reason,
    })
}

fn write_event(state_dir: &Path, event: &CtrlCEvent) -> io::Result<()> {
    let dir = events_dir(state_dir);
    fs::create_dir_all(&dir)?;
    let filename = format!("{:013}-{}.json", event.exit_at_ms, event.pid);
    let path = dir.join(filename);
    let bytes = serde_json::to_vec(event)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    fs::write(&path, bytes)?;
    prune_old_events(&dir, MAX_RETAINED_EVENTS);
    Ok(())
}

pub fn events_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(EVENTS_DIRNAME)
}

/// Read newest-first up to `limit` events from `<state_dir>/ctrl_c_events/`.
/// Used by the dashboard. Missing dir → empty Vec.
pub fn read_recent_events(state_dir: &Path, limit: usize) -> Vec<CtrlCEvent> {
    let dir = events_dir(state_dir);
    let entries = match fs::read_dir(&dir) {
        Ok(it) => it,
        Err(_) => return Vec::new(),
    };
    let mut events: Vec<CtrlCEvent> = entries
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .filter_map(|e| {
            let bytes = fs::read(e.path()).ok()?;
            serde_json::from_slice::<CtrlCEvent>(&bytes).ok()
        })
        .collect();
    events.sort_by(|a, b| b.exit_at_ms.cmp(&a.exit_at_ms));
    events.truncate(limit);
    events
}

fn prune_old_events(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                return None;
            }
            let mtime = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            Some((mtime, path))
        })
        .collect();
    if files.len() <= keep {
        return;
    }
    // Newest first; keep the head, delete the rest.
    files.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in files.into_iter().skip(keep) {
        let _ = fs::remove_file(path);
    }
}

fn unix_millis_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// **Test-only.** Reset the observation + handoff state so tests that
/// exercise `build_event` / `flush_on_exit` can simulate a fresh
/// process. Real processes only ever transition once per run.
#[cfg(test)]
pub(crate) fn reset_for_test() {
    OBSERVED_UNIX_MS.store(0, Ordering::SeqCst);
    if let Ok(mut g) = HANDOFF_OUTCOME.lock() {
        *g = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::tempdir;

    /// Tests that mutate `OBSERVED_UNIX_MS` / `HANDOFF_OUTCOME` would
    /// race each other under cargo's default parallel runner because
    /// the module-level statics are process-global. Acquire this lock
    /// at the top of every test that touches `record_observed`,
    /// `record_handoff`, `reset_for_test`, `build_event`, or
    /// `flush_on_exit` to serialize them deterministically.
    static STATE_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn invocation_kind_str_round_trips() {
        assert_eq!(InvocationKind::Direct.as_str(), "direct");
        assert_eq!(InvocationKind::Attach.as_str(), "attach");
        assert_eq!(InvocationKind::Centralized.as_str(), "centralized");
    }

    #[test]
    fn ctrl_c_event_round_trips_through_json() {
        let event = CtrlCEvent {
            pid: 1234,
            observed_at_ms: 1_700_000_000_000,
            exit_at_ms: 1_700_000_000_250,
            elapsed_ms: 250,
            kind: InvocationKind::Direct,
            exit_code: 130,
            cwd: Some("/tmp/x".to_string()),
            handed_off: Some(true),
            handoff_reason: Some("ctrl_c_subprocess".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""kind":"direct""#));
        assert!(json.contains(r#""elapsed_ms":250"#));
        assert!(json.contains(r#""handed_off":true"#));
        assert!(json.contains(r#""handoff_reason":"ctrl_c_subprocess""#));
        let back: CtrlCEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pid, 1234);
        assert_eq!(back.elapsed_ms, 250);
        assert_eq!(back.kind, InvocationKind::Direct);
        assert_eq!(back.cwd.as_deref(), Some("/tmp/x"));
        assert_eq!(back.handed_off, Some(true));
        assert_eq!(back.handoff_reason.as_deref(), Some("ctrl_c_subprocess"));
    }

    #[test]
    fn ctrl_c_event_parses_legacy_files_without_handoff_fields() {
        // Pre-issue-#285 event files have no `handed_off` / `handoff_reason`
        // fields. `#[serde(default)]` must make them parse cleanly so the
        // dashboard doesn't lose history when the binary is upgraded.
        let legacy = r#"{
            "pid": 1234,
            "observed_at_ms": 1700000000000,
            "exit_at_ms": 1700000000250,
            "elapsed_ms": 250,
            "kind": "direct",
            "exit_code": 130,
            "cwd": "/tmp/x"
        }"#;
        let event: CtrlCEvent = serde_json::from_str(legacy).unwrap();
        assert_eq!(event.pid, 1234);
        assert_eq!(event.handed_off, None);
        assert_eq!(event.handoff_reason, None);
    }

    #[test]
    fn read_recent_events_returns_empty_when_dir_missing() {
        let tmp = tempdir().unwrap();
        let events = read_recent_events(tmp.path(), 10);
        assert!(events.is_empty());
    }

    #[test]
    fn read_recent_events_returns_newest_first_and_respects_limit() {
        let tmp = tempdir().unwrap();
        let dir = events_dir(tmp.path());
        fs::create_dir_all(&dir).unwrap();
        for i in 0..5u64 {
            let event = CtrlCEvent {
                pid: 100 + i as u32,
                observed_at_ms: 1_700_000_000_000 + i * 1000,
                exit_at_ms: 1_700_000_000_500 + i * 1000,
                elapsed_ms: 500,
                kind: InvocationKind::Direct,
                exit_code: 130,
                cwd: None,
                handed_off: None,
                handoff_reason: None,
            };
            let path = dir.join(format!("{:013}-{}.json", event.exit_at_ms, event.pid));
            fs::write(&path, serde_json::to_vec(&event).unwrap()).unwrap();
        }
        let events = read_recent_events(tmp.path(), 3);
        assert_eq!(events.len(), 3);
        // Newest first means the largest exit_at_ms comes first.
        assert_eq!(events[0].exit_at_ms, 1_700_000_000_500 + 4_000);
        assert_eq!(events[1].exit_at_ms, 1_700_000_000_500 + 3_000);
        assert_eq!(events[2].exit_at_ms, 1_700_000_000_500 + 2_000);
    }

    #[test]
    fn read_recent_events_skips_unparseable_files() {
        let tmp = tempdir().unwrap();
        let dir = events_dir(tmp.path());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("garbage.json"), b"not json").unwrap();
        let good = CtrlCEvent {
            pid: 1,
            observed_at_ms: 100,
            exit_at_ms: 200,
            elapsed_ms: 100,
            kind: InvocationKind::Attach,
            exit_code: 130,
            cwd: None,
            handed_off: Some(true),
            handoff_reason: None,
        };
        fs::write(dir.join("good.json"), serde_json::to_vec(&good).unwrap()).unwrap();
        let events = read_recent_events(tmp.path(), 10);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].pid, 1);
        assert_eq!(events[0].handed_off, Some(true));
    }

    #[test]
    fn prune_old_events_keeps_newest() {
        let tmp = tempdir().unwrap();
        let dir = events_dir(tmp.path());
        fs::create_dir_all(&dir).unwrap();
        // Create 10 files with monotonically-increasing mtime by writing
        // them in order; on most filesystems that's enough to differentiate.
        for i in 0..10u64 {
            let path = dir.join(format!("evt-{i:02}.json"));
            fs::write(&path, b"{}").unwrap();
            // Tiny sleep so mtimes can differ on coarse-grained filesystems.
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        prune_old_events(&dir, 3);
        let remaining = fs::read_dir(&dir).unwrap().count();
        assert_eq!(remaining, 3, "prune must keep exactly the cap");
    }

    #[test]
    fn flush_on_exit_is_noop_when_never_observed() {
        let _guard = STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Reset so prior tests in this module can't pollute the static
        // observation point. After reset, `was_observed` must be false
        // and `flush_on_exit` must write nothing.
        reset_for_test();
        let tmp = tempdir().unwrap();
        flush_on_exit(tmp.path(), InvocationKind::Direct, 0);
        let dir = events_dir(tmp.path());
        if dir.exists() {
            // Directory creation only happens inside write_event; if it
            // exists, no event files should be inside.
            let count = fs::read_dir(&dir).unwrap().count();
            assert_eq!(count, 0);
        }
    }

    /// Issue #285 rec 1: every Ctrl+C must re-stamp the observation
    /// point. The prior `OnceLock` design only stamped the first press,
    /// so a user who pressed Ctrl+C once mid-session would see the
    /// entire intervening time attributed to the eventual shutdown.
    #[test]
    fn record_observed_updates_on_every_call() {
        let _guard = STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        record_observed();
        let first = OBSERVED_UNIX_MS.load(Ordering::SeqCst);
        assert!(first > 0, "first observation must stamp");
        // Sleep long enough that the wall clock advances at least 1ms
        // even on coarse-grained Windows timers (typically 15ms tick).
        std::thread::sleep(std::time::Duration::from_millis(20));
        record_observed();
        let second = OBSERVED_UNIX_MS.load(Ordering::SeqCst);
        assert!(
            second > first,
            "second observation must overwrite the first (got {second} vs {first})"
        );
    }

    /// Issue #285 rec 2: the handoff outcome recorded by the teardown
    /// site must propagate into the event file so the dashboard can
    /// distinguish "daemon adopted" from "synchronous fallback".
    #[test]
    fn record_handoff_propagates_to_event() {
        let _guard = STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        record_observed();
        record_handoff(true, Some("ctrl_c_subprocess"));
        let event = build_event(InvocationKind::Direct, 130).expect("event built");
        assert_eq!(event.handed_off, Some(true));
        assert_eq!(event.handoff_reason.as_deref(), Some("ctrl_c_subprocess"));
    }

    #[test]
    fn record_handoff_failure_surfaces_reason() {
        let _guard = STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        record_observed();
        record_handoff(false, Some("daemon_unreachable"));
        let event = build_event(InvocationKind::Direct, 130).expect("event built");
        assert_eq!(event.handed_off, Some(false));
        assert_eq!(event.handoff_reason.as_deref(), Some("daemon_unreachable"));
    }

    #[test]
    fn build_event_without_handoff_leaves_fields_none() {
        let _guard = STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // When neither teardown site fires (e.g. `clud --no-daemon` exits
        // before reaching the teardown helper), the event must still be
        // written but with the handoff fields left as None so the
        // dashboard can show "unknown" rather than claiming a fast path.
        reset_for_test();
        record_observed();
        let event = build_event(InvocationKind::Direct, 130).expect("event built");
        assert_eq!(event.handed_off, None);
        assert_eq!(event.handoff_reason, None);
    }
}
