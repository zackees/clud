//! Per-phase startup events for `run_daemon` (#2333).
//!
//! Between `daemon_starting` and `daemon_started` the daemon does several
//! steps that can be slow or fail. [`StartupPhases::mark`] appends one
//! `daemon_startup_phase` event per finished step (`phase`, `elapsed_ms` since
//! start) and [`StartupPhases::fail`] records `daemon_start_failed` plus the
//! stderr line in `daemon.stderr.log`, so the cause survives a detached
//! daemon whose stderr is `/dev/null`. Both reuse the existing event append.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;

use super::daemon_events;

const STDERR_LOG_NAME: &str = "daemon.stderr.log";
const MAX_STDERR_LOG_BYTES: u64 = 256 * 1024;

pub(super) struct StartupPhases<'a> {
    state_dir: &'a Path,
    started: Instant,
}

impl<'a> StartupPhases<'a> {
    pub(super) fn new(state_dir: &'a Path, started: Instant) -> Self {
        Self { state_dir, started }
    }

    /// Record that `phase` just finished.
    pub(super) fn mark(&self, phase: &'static str) {
        daemon_events::log_event(
            self.state_dir,
            "daemon_startup_phase",
            [
                ("phase", json!(phase)),
                ("elapsed_ms", json!(self.started.as_millis_u64())),
            ],
        );
    }

    /// Report an early exit: stderr, `daemon.stderr.log`, and a
    /// `daemon_start_failed` event. Returns the exit code (1).
    pub(super) fn fail(&self, phase: &'static str, message: &str) -> i32 {
        eprintln!("{message}");
        append_stderr_log(&self.state_dir.join(STDERR_LOG_NAME), message);
        daemon_events::log_event(
            self.state_dir,
            "daemon_start_failed",
            [
                ("phase", json!(phase)),
                ("elapsed_ms", json!(self.started.as_millis_u64())),
                ("error", json!(message)),
            ],
        );
        1
    }
}

trait ElapsedMs {
    fn as_millis_u64(&self) -> u64;
}

impl ElapsedMs for Instant {
    fn as_millis_u64(&self) -> u64 {
        u64::try_from(self.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// Append one line, rolling to a single `.1` backup past the size cap.
fn append_stderr_log(path: &Path, message: &str) {
    if fs::metadata(path).is_ok_and(|m| m.len() >= MAX_STDERR_LOG_BYTES) {
        let mut backup = PathBuf::from(path);
        backup.set_extension("log.1");
        let _ = fs::remove_file(&backup);
        let _ = fs::rename(path, backup);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::paths::daemon_events_path;

    fn events(dir: &Path) -> Vec<serde_json::Value> {
        fs::read_to_string(daemon_events_path(dir))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn phases_are_logged_in_order_with_monotonic_elapsed() {
        let tmp = tempfile::tempdir().unwrap();
        let phases = StartupPhases::new(tmp.path(), Instant::now());
        for name in ["a", "b", "c"] {
            phases.mark(name);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let got = events(tmp.path());
        let names: Vec<_> = got.iter().map(|e| e["phase"].as_str().unwrap()).collect();
        assert_eq!(names, ["a", "b", "c"]);
        let ms: Vec<u64> = got
            .iter()
            .map(|e| e["elapsed_ms"].as_u64().unwrap())
            .collect();
        assert!(ms.windows(2).all(|w| w[0] <= w[1]), "{ms:?}");
        assert!(got.iter().all(|e| e["op"] == "daemon_startup_phase"));
    }

    #[test]
    fn fail_records_event_and_stderr_log() {
        let tmp = tempfile::tempdir().unwrap();
        let phases = StartupPhases::new(tmp.path(), Instant::now());
        assert_eq!(phases.fail("loopback_bind", "boom"), 1);
        let got = events(tmp.path());
        assert_eq!(got[0]["op"], "daemon_start_failed");
        assert_eq!(got[0]["phase"], "loopback_bind");
        assert_eq!(got[0]["error"], "boom");
        let log = fs::read_to_string(tmp.path().join(STDERR_LOG_NAME)).unwrap();
        assert_eq!(log, "boom\n");
    }

    #[test]
    fn stderr_log_rolls_past_the_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(STDERR_LOG_NAME);
        fs::write(&path, vec![b'x'; MAX_STDERR_LOG_BYTES as usize]).unwrap();
        append_stderr_log(&path, "fresh");
        assert_eq!(fs::read_to_string(&path).unwrap(), "fresh\n");
        assert!(tmp.path().join("daemon.stderr.log.1").exists());
    }
}
