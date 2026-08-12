//! Always-on, bounded forensic log for the Codex bridge (#772).
//!
//! Only failures and retry decisions are recorded. Request/response bodies,
//! credentials, bearer tokens, and upstream URLs are deliberately absent from
//! this API so callers cannot persist them accidentally.
//! Unit tests and binaries marked with `CLUD_INTEGRATION_TESTS=1` write under
//! `test-sessions/`, keeping fixture failures out of production diagnostics.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const FLUSH_LINES: usize = 64;
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
pub const DEFAULT_MAX_BYTES: usize = 1024 * 1024;
const INTEGRATION_TESTS_ENV: &str = "CLUD_INTEGRATION_TESTS";
const PRODUCTION_SESSIONS_DIR: &str = "sessions";
const TEST_SESSIONS_DIR: &str = "test-sessions";

#[derive(Debug)]
pub struct BridgeLog {
    path: PathBuf,
    buffered: Vec<String>,
    buffered_bytes: usize,
    written_bytes: usize,
    max_bytes: usize,
    last_flush: Instant,
    disabled: bool,
    truncated: bool,
    recorded: bool,
}

impl BridgeLog {
    pub fn new(path: PathBuf) -> Self {
        Self::with_max_bytes(path, DEFAULT_MAX_BYTES)
    }

    pub fn with_max_bytes(path: PathBuf, max_bytes: usize) -> Self {
        Self {
            path,
            buffered: Vec::new(),
            buffered_bytes: 0,
            written_bytes: 0,
            max_bytes,
            last_flush: Instant::now(),
            disabled: false,
            truncated: false,
            recorded: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn has_records(&self) -> bool {
        self.recorded
    }

    pub fn record(&mut self, event: serde_json::Value) {
        if self.disabled || self.truncated {
            return;
        }
        let line = event.to_string();
        let line_bytes = line.len() + 1;
        if self.written_bytes + self.buffered_bytes + line_bytes > self.max_bytes {
            self.record_truncation();
            return;
        }
        self.recorded = true;
        self.buffered_bytes += line_bytes;
        self.buffered.push(line);
        if self.buffered.len() >= FLUSH_LINES || self.last_flush.elapsed() >= FLUSH_INTERVAL {
            self.flush();
        }
    }

    fn record_truncation(&mut self) {
        self.truncated = true;
        self.recorded = true;
        let line = serde_json::json!({
            "ts_ms": unix_ms(),
            "event": "truncated",
            "max_bytes": self.max_bytes,
        })
        .to_string();
        self.buffered_bytes += line.len() + 1;
        self.buffered.push(line);
        self.flush();
    }

    pub fn flush(&mut self) {
        if self.disabled || self.buffered.is_empty() {
            return;
        }
        let body = format!("{}\n", self.buffered.join("\n"));
        if self.append(&body).is_err() {
            self.disabled = true;
        } else {
            self.written_bytes += body.len();
        }
        self.buffered.clear();
        self.buffered_bytes = 0;
        self.last_flush = Instant::now();
    }

    fn append(&self, body: &str) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(body.as_bytes())
    }
}

impl Drop for BridgeLog {
    fn drop(&mut self) {
        self.flush();
    }
}

pub fn session_bridge_log_path(
    state_dir: &Path,
    session_pid: u32,
    session_start_epoch: u64,
) -> PathBuf {
    forensic_sessions_dir(state_dir)
        .join(format!("{session_pid}__{session_start_epoch}"))
        .join("bridge.jsonl")
}

/// Root for foreground forensic logs. Test-mode processes use a sibling tree
/// so their synthetic failures cannot be mistaken for production incidents.
pub fn forensic_sessions_dir(state_dir: &Path) -> PathBuf {
    forensic_sessions_dir_for_mode(state_dir, test_log_mode())
}

fn test_log_mode() -> bool {
    test_log_mode_from(
        cfg!(test),
        std::env::var_os(INTEGRATION_TESTS_ENV).as_deref(),
    )
}

fn test_log_mode_from(test_build: bool, integration_tests: Option<&OsStr>) -> bool {
    test_build || integration_tests.is_some_and(|value| value == "1")
}

fn forensic_sessions_dir_for_mode(state_dir: &Path, test_mode: bool) -> PathBuf {
    state_dir.join(if test_mode {
        TEST_SESSIONS_DIR
    } else {
        PRODUCTION_SESSIONS_DIR
    })
}

pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_log_creates_no_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bridge.jsonl");
        drop(BridgeLog::new(path.clone()));
        assert!(!path.exists());
    }

    #[test]
    fn concurrent_callers_produce_complete_json_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bridge.jsonl");
        let log = std::sync::Arc::new(std::sync::Mutex::new(BridgeLog::new(path.clone())));
        let mut workers = Vec::new();
        for worker in 0..16 {
            let log = std::sync::Arc::clone(&log);
            workers.push(std::thread::spawn(move || {
                for event in 0..20 {
                    log.lock()
                        .unwrap()
                        .record(serde_json::json!({"worker": worker, "event": event}));
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        drop(log);
        let text = fs::read_to_string(path).unwrap();
        assert_eq!(text.lines().count(), 320);
        for line in text.lines() {
            serde_json::from_str::<serde_json::Value>(line).unwrap();
        }
    }

    #[test]
    fn byte_cap_leaves_visible_truncation_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bridge.jsonl");
        let mut log = BridgeLog::with_max_bytes(path.clone(), 100);
        for value in 0..100 {
            log.record(serde_json::json!({"event": "failure", "value": value}));
        }
        drop(log);
        let text = fs::read_to_string(path).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.last().unwrap()["event"], "truncated");
        assert_eq!(lines.last().unwrap()["max_bytes"], 100);
    }

    #[test]
    fn unit_test_log_path_is_isolated_from_production_sessions() {
        assert_eq!(
            session_bridge_log_path(Path::new("/state"), 42, 1700000000),
            Path::new("/state")
                .join("test-sessions")
                .join("42__1700000000")
                .join("bridge.jsonl")
        );
    }

    #[test]
    fn production_and_test_forensic_roots_are_disjoint() {
        let state = Path::new("/state");
        assert_eq!(
            forensic_sessions_dir_for_mode(state, false),
            state.join("sessions")
        );
        assert_eq!(
            forensic_sessions_dir_for_mode(state, true),
            state.join("test-sessions")
        );
    }

    #[test]
    fn integration_test_marker_selects_the_test_log_tree() {
        assert!(!test_log_mode_from(false, None));
        assert!(!test_log_mode_from(false, Some(OsStr::new("0"))));
        assert!(test_log_mode_from(false, Some(OsStr::new("1"))));
        assert!(test_log_mode_from(true, None));
    }
}
