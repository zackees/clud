//! Pre-deletion audit trail for clud's destructive GC/sweep paths (issue
//! #893).
//!
//! #893 reported a live repo directory vanishing mid-session with nothing in
//! any log naming what removed it. Whatever the culprit turns out to be, the
//! durable fix has two halves and this module is one of them: every deletion
//! path that could touch user data writes one JSONL line to
//! `<state>/gc-audit.jsonl` **immediately before** acting, naming the sweep
//! site, the exact filesystem path, and the rule that selected it. A future
//! incident is then answerable by grepping one file instead of being
//! reconstructed from an unrelated daemon crash log.
//!
//! Scope boundary: sites that remove only directories clud itself created
//! moments earlier inside the same call (the soldr-download temp dir, the
//! cmd-installer staging dir, the git-bash extraction dir, voice capture
//! dirs) are deliberately not wired — they cannot match user data by
//! construction.
//!
//! Synchronous append is deliberate, unlike the reaper log's buffering
//! ([`crate::reap_log`]): deletions here are rare (daily sweeps), while a
//! crash mid-deletion is exactly when the line matters most, so per-line
//! flush cost buys durability where it counts. All errors are swallowed —
//! auditing must never block or fail the sweep itself.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::daemon::ENV_STATE_DIR;

/// Audit log file name, resolved against the same state dir the daemon uses.
pub const AUDIT_LOG_FILE: &str = "gc-audit.jsonl";

/// Resolve `<state-dir>/gc-audit.jsonl`, honoring [`ENV_STATE_DIR`] the same
/// way `daemon::paths::default_state_dir` does. `None` when no home dir can
/// be resolved (headless/misconfigured env) — the sweep then runs unaudited.
pub fn audit_log_path() -> Option<PathBuf> {
    let state_dir = match std::env::var(ENV_STATE_DIR) {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => home_dir()?.join(".clud").join("state"),
    };
    Some(state_dir.join(AUDIT_LOG_FILE))
}

/// Record one pending deletion. Best-effort: never panics, never errors.
///
/// Call sites must invoke this *before* the destructive call, per #893.
///
/// - `site` — the code path performing the deletion (e.g. `gc.target-sweep`).
/// - `path` — the exact directory about to be removed.
/// - `rule` — why it was selected (e.g. `target-sweep stale>14d`).
pub fn record(site: &str, path: &Path, rule: &str) {
    let Some(log_path) = audit_log_path() else {
        return;
    };
    record_at(&log_path, site, path, rule);
}

/// Testable core of [`record`] — appends one JSONL line synchronously.
pub fn record_at(log_path: &Path, site: &str, path: &Path, rule: &str) {
    let line = serde_json::json!({
        "ts_ms": SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        "event": "delete",
        "site": site,
        "path": path.display().to_string(),
        "rule": rule,
    });
    if let Some(parent) = log_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log_path) else {
        return;
    };
    let _ = file.write_all(format!("{line}\n").as_bytes());
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(path) = std::env::var_os("USERPROFILE") {
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }
    if let Some(path) = std::env::var_os("HOME") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

/// Shared test helper: swap [`ENV_STATE_DIR`] for the duration of a test so
/// sweeps can be pointed at an injectable audit log. `std::env` is
/// process-global, so all users serialize through one mutex — tests asserting
/// on the injected file must tolerate unrelated concurrent deletions landing
/// there too (assert on a *matching* line, not an exact count).
#[cfg(test)]
pub(crate) struct StateDirGuard {
    prior: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl StateDirGuard {
    pub(crate) fn set(dir: &Path) -> Self {
        static M: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        let lock = M
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let prior = std::env::var_os(ENV_STATE_DIR);
        std::env::set_var(ENV_STATE_DIR, dir);
        Self { prior, _lock: lock }
    }
}

#[cfg(test)]
impl Drop for StateDirGuard {
    fn drop(&mut self) {
        match self.prior.take() {
            Some(v) => std::env::set_var(ENV_STATE_DIR, v),
            None => std::env::remove_var(ENV_STATE_DIR),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn record_at_writes_parseable_jsonl_with_site_path_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("gc-audit.jsonl");
        record_at(
            &log,
            "gc.target-sweep",
            Path::new("/dev/repo/target"),
            "stale>14d",
        );
        let body = fs::read_to_string(&log).unwrap();
        assert_eq!(body.lines().count(), 1);
        let parsed: Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(parsed["event"], "delete");
        assert_eq!(parsed["site"], "gc.target-sweep");
        // Forward slashes survive round-trip; the display form is what makes
        // the path greppable against `dir /s` or `find` output.
        assert!(parsed["path"].as_str().unwrap().contains("repo"));
        assert!(parsed["path"].as_str().unwrap().ends_with("target"));
        assert_eq!(parsed["rule"], "stale>14d");
        assert!(parsed["ts_ms"].as_u64().is_some());
    }

    #[test]
    fn successive_records_append_one_line_each() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("nested").join("gc-audit.jsonl");
        record_at(&log, "a", Path::new("/x"), "r1");
        record_at(&log, "b", Path::new("/y"), "r2");
        let body = fs::read_to_string(&log).unwrap();
        assert_eq!(body.lines().count(), 2, "parent dirs auto-created");
    }

    #[test]
    fn an_unwritable_log_is_silent_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let blocker = tmp.path().join("blocker");
        fs::write(&blocker, b"x").unwrap();
        // Parent is an existing *file* — open can never succeed.
        record_at(&blocker.join("gc-audit.jsonl"), "a", Path::new("/x"), "r");
    }

    #[test]
    fn audit_log_path_honors_the_daemon_state_dir_env() {
        let tmp = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::set(tmp.path());
        let resolved = audit_log_path().expect("env override resolves without home");
        assert_eq!(
            resolved.file_name().unwrap(),
            AUDIT_LOG_FILE,
            "same state dir convention as the daemon"
        );
        assert!(resolved.starts_with(tmp.path()));
    }
}
