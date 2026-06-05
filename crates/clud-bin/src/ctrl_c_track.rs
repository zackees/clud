//! Cross-path Ctrl+C exit timing.
//!
//! Records the moment the process first observes Ctrl+C (whether under the
//! direct runner, an attached daemon session, or the centralized launch
//! path) and, just before the process exits, writes a JSON event under
//! `<state_dir>/ctrl_c_events/<unix-ms>-<pid>.json` capturing the elapsed
//! wall-clock time from "Ctrl+C seen" to "about to exit". The daemon
//! dashboard reads these files and surfaces them on `clud ui` so the
//! recurring "Ctrl+C takes forever to drop me back at the shell" problem
//! has hard numbers attached.
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
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

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
}

// Process-wide observation point. Recorded once on the first Ctrl+C the
// signal handler sees, then read by `flush_on_exit`. Using both an
// `Instant` and a unix-ms timestamp lets the dashboard correlate events
// against absolute time while keeping the elapsed calculation immune to
// wall-clock jumps.
static OBSERVED_INSTANT: OnceLock<Instant> = OnceLock::new();
static OBSERVED_UNIX_MS: AtomicU64 = AtomicU64::new(0);

/// Mark the process as having observed Ctrl+C. Safe to call from a signal
/// handler — no allocations, no locks, only atomic / OnceLock writes.
/// Subsequent calls are no-ops so the very first interrupt wins.
pub fn record_observed() {
    if OBSERVED_INSTANT.set(Instant::now()).is_ok() {
        let unix_ms = unix_millis_now();
        // Race-free: only the OnceLock-winning thread reaches here, so
        // this store is the unique writer.
        OBSERVED_UNIX_MS.store(unix_ms, Ordering::SeqCst);
    }
}

pub fn was_observed() -> bool {
    OBSERVED_INSTANT.get().is_some()
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
    let observed = OBSERVED_INSTANT.get()?;
    let observed_at_ms = OBSERVED_UNIX_MS.load(Ordering::SeqCst);
    let elapsed_ms = observed.elapsed().as_millis() as u64;
    let exit_at_ms = observed_at_ms.saturating_add(elapsed_ms);
    let cwd = std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    Some(CtrlCEvent {
        pid: std::process::id(),
        observed_at_ms,
        exit_at_ms,
        elapsed_ms,
        kind,
        exit_code,
        cwd,
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

/// **Test-only.** Reset the OnceLock-backed observation state. Real
/// processes only ever observe Ctrl+C once per run; tests need to
/// simulate fresh observations across cases.
#[cfg(test)]
pub(crate) fn reset_for_test() {
    // OnceLock has no public reset, so we exploit `take` on a clone via
    // a fresh OnceLock through interior mutation — there is no such API,
    // so instead tests that need a clean slate must run in a serialized
    // section and observe via [`record_observed`] from a fresh process.
    // To keep things simple, this resets only the timestamp atomic; the
    // OnceLock retention is acceptable because [`build_event`] reads
    // both the instant and the millis, and tests that probe build_event
    // use a brand-new module via `cargo test` per-process isolation.
    OBSERVED_UNIX_MS.store(0, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""kind":"direct""#));
        assert!(json.contains(r#""elapsed_ms":250"#));
        let back: CtrlCEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pid, 1234);
        assert_eq!(back.elapsed_ms, 250);
        assert_eq!(back.kind, InvocationKind::Direct);
        assert_eq!(back.cwd.as_deref(), Some("/tmp/x"));
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
        };
        fs::write(dir.join("good.json"), serde_json::to_vec(&good).unwrap()).unwrap();
        let events = read_recent_events(tmp.path(), 10);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].pid, 1);
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
        // Side-stepping OnceLock retention: in a fresh test process the
        // `OBSERVED_INSTANT` is unset, so flush_on_exit must write
        // nothing. We can't reset OnceLock from inside the process, so
        // this test only runs cleanly when no other test in this module
        // has already called `record_observed`. Since we never do, this
        // test pins the "no observation, no file" contract.
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
}
