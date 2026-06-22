//! `clud log` (issue #469): POST one telemetry event to the daemon's
//! HTTP server.
//!
//! Beta-prototype scope. The logger captures its own PPID + CWD +
//! `CLUD_*` env vars and ships them to `$CLUD_DAEMON_HTTP_SERVER`. With
//! `--fail-on-no-server` the command exits non-zero if either the env
//! var is unset or the POST round-trip fails — the integration test in
//! `tests/telemetry_endpoint.rs` uses that flag to prove a real send.
//! Without the flag, failures are swallowed (exit 0) so the eventual
//! hook caller never breaks because the daemon is down.
//!
//! Discovery happens via the environment, not a fixed port: callers
//! (the daemon, the test harness) export `CLUD_DAEMON_HTTP_SERVER=
//! http://127.0.0.1:<port>` before invoking `clud log`.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Env-var pointing at the daemon's HTTP listener. Full URL — e.g.
/// `http://127.0.0.1:54321`. Documented in `daemon.json` once #469 wires
/// it into the daemon-info file (out of scope for this PR; for now the
/// caller is responsible for exporting it).
pub const ENV_DAEMON_HTTP_SERVER: &str = "CLUD_DAEMON_HTTP_SERVER";

/// HTTP POST timeout for the logger. Tight on purpose — hook callers
/// must not block on a stuck daemon.
const POST_TIMEOUT: Duration = Duration::from_secs(2);

/// Payload posted to `POST /telemetry/log` on the daemon. Mirrors the
/// `TelemetryEntry` shape on the receiving side, minus the daemon-added
/// `received_at_ms` field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryPayload {
    pub parent_pid: u32,
    pub time_ms: u64,
    pub cmd: String,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
}

/// Run `clud log`. Returns the process exit code so `main.rs` can
/// `std::process::exit` cleanly.
pub fn run(cmd: &str, fail_on_no_server: bool) -> i32 {
    let server = std::env::var(ENV_DAEMON_HTTP_SERVER).ok();
    let server = match server {
        Some(s) if !s.is_empty() => s,
        _ => {
            if fail_on_no_server {
                eprintln!("[clud log] ${ENV_DAEMON_HTTP_SERVER} is unset; cannot post telemetry");
                return 2;
            }
            return 0;
        }
    };

    let payload = build_payload(cmd);
    let url = format!("{}/telemetry/log", server.trim_end_matches('/'));

    let body = match serde_json::to_vec(&payload) {
        Ok(b) => b,
        Err(err) => {
            if fail_on_no_server {
                eprintln!("[clud log] serialize failed: {err}");
                return 3;
            }
            return 0;
        }
    };

    let result = ureq::AgentBuilder::new()
        .timeout(POST_TIMEOUT)
        .build()
        .post(&url)
        .set("Content-Type", "application/json")
        .send_bytes(&body);

    match result {
        Ok(resp) if (200..300).contains(&resp.status()) => 0,
        Ok(resp) => {
            if fail_on_no_server {
                eprintln!("[clud log] {url} returned {}", resp.status());
                return 4;
            }
            0
        }
        Err(err) => {
            if fail_on_no_server {
                eprintln!("[clud log] {url} failed: {err}");
                return 5;
            }
            0
        }
    }
}

fn build_payload(cmd: &str) -> TelemetryPayload {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let time_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let env: BTreeMap<String, String> = std::env::vars()
        .filter(|(k, _)| k.starts_with("CLUD_"))
        .collect();
    TelemetryPayload {
        parent_pid: parent_pid(),
        time_ms,
        cmd: cmd.to_string(),
        cwd,
        env,
    }
}

/// Parent PID of the current process — i.e., the PID of whatever
/// invoked `clud log`. On Unix this is `getppid()`. On Windows we walk
/// the toolhelp32 snapshot for the entry whose `th32ProcessID` matches
/// our own PID and read its `th32ParentProcessID`.
#[cfg(unix)]
fn parent_pid() -> u32 {
    // SAFETY: `getppid` is always-safe libc.
    unsafe { libc::getppid() as u32 }
}

#[cfg(windows)]
fn parent_pid() -> u32 {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let me = std::process::id();
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[Pid::from_u32(me)]),
        true,
        ProcessRefreshKind::nothing(),
    );
    sys.process(Pid::from_u32(me))
        .and_then(|p| p.parent())
        .map(|p| p.as_u32())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_captures_clud_env_vars_only() {
        // Hermetic — uses temporary env mutations gated by a serial guard
        // so parallel tests don't flake on shared process env.
        let lock = ENV_LOCK.lock().unwrap();
        let prev_marker = std::env::var("CLUD_TEST_MARKER_ONLY").ok();
        let prev_other = std::env::var("PATH_LIKE_FOO").ok();
        // SAFETY: env writes inside a process-wide serial lock; restored below.
        unsafe {
            std::env::set_var("CLUD_TEST_MARKER_ONLY", "42");
            std::env::set_var("PATH_LIKE_FOO", "ignored");
        }
        let p = build_payload("echo hi");
        assert_eq!(p.cmd, "echo hi");
        assert_eq!(
            p.env.get("CLUD_TEST_MARKER_ONLY").map(String::as_str),
            Some("42")
        );
        assert!(p.env.keys().all(|k| k.starts_with("CLUD_")));
        // Cleanup.
        unsafe {
            std::env::remove_var("CLUD_TEST_MARKER_ONLY");
            if let Some(v) = prev_marker {
                std::env::set_var("CLUD_TEST_MARKER_ONLY", v);
            }
            std::env::remove_var("PATH_LIKE_FOO");
            if let Some(v) = prev_other {
                std::env::set_var("PATH_LIKE_FOO", v);
            }
        }
        drop(lock);
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
