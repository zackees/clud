//! Crash-report writer for clud.
//!
//! Installs a process panic hook from the foreground CLI, daemon, and worker
//! entry points. On panic, captures `Backtrace::force_capture()` plus the
//! panic site and writes a JSON record to
//! `~/.clud/state/crashes/<unix_ms>-<role>-<pid>.json` before chaining to the
//! previously-installed hook so stderr behavior is preserved.
//!
//! The first `install(role)` call also surfaces a one-line stderr notice if
//! there's a crash report newer than the last one this process tree saw, so
//! the next launch tells the user something happened without spamming on
//! every subsequent launch.
//!
//! Role can be updated by a later `install(role)` call (e.g. main.rs installs
//! as `"foreground"`, then the daemon process re-installs as `"daemon"`
//! before doing daemon work); the underlying hook is installed only once, so
//! a crash inside the daemon writes one report tagged `"daemon"`, not two.

use std::backtrace::Backtrace;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

pub(crate) const MAX_REPORTS: usize = 50;
const LAST_SEEN_FILE: &str = "last_seen";

static CURRENT_ROLE: OnceLock<RwLock<String>> = OnceLock::new();
static HOOK_INSTALLED: OnceLock<()> = OnceLock::new();

#[derive(Serialize)]
struct CrashReport {
    version: &'static str,
    role: String,
    pid: u32,
    cwd: Option<String>,
    args: Vec<String>,
    timestamp_unix_ms: u128,
    panic_location: Option<String>,
    panic_message: String,
    backtrace: String,
}

/// Install (or update) the clud crash reporter for this process.
///
/// Called from `main.rs` (`role = "foreground"`), the daemon process entry
/// (`role = "daemon"`), and the worker process entry (`role = "worker"`).
/// Idempotent: the panic hook itself is installed exactly once per process;
/// subsequent calls only update the role the hook will tag reports with.
pub fn install(role: &str) {
    let lock = CURRENT_ROLE.get_or_init(|| RwLock::new(role.to_string()));
    if let Ok(mut w) = lock.write() {
        *w = role.to_string();
    }

    HOOK_INSTALLED.get_or_init(|| {
        if let Ok(dir) = crashes_dir() {
            if let Some((_, path)) = surface_previous_report(&dir) {
                eprintln!("clud: previous crash report at {}", path.display());
            }
        }
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let role = current_role();
            let _ = write_panic_report(&role, info);
            prev_hook(info);
        }));
    });
}

fn current_role() -> String {
    CURRENT_ROLE
        .get()
        .and_then(|lock| lock.read().ok().map(|g| g.clone()))
        .unwrap_or_else(|| "unknown".to_string())
}

fn crashes_dir() -> std::io::Result<PathBuf> {
    let state_dir = crate::daemon::default_state_dir()?;
    let dir = state_dir.join("crashes");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn write_panic_report(
    role: &str,
    info: &std::panic::PanicHookInfo<'_>,
) -> std::io::Result<PathBuf> {
    let dir = crashes_dir()?;
    let pid = std::process::id();
    let ts = now_unix_ms();
    let path = dir.join(format!("{ts}-{role}-{pid}.json"));

    let panic_location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
    let panic_message = panic_payload_to_string(info);
    let backtrace = Backtrace::force_capture().to_string();
    let cwd = std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    let args = sanitize_args(std::env::args().collect());

    let report = CrashReport {
        version: env!("CARGO_PKG_VERSION"),
        role: role.to_string(),
        pid,
        cwd,
        args,
        timestamp_unix_ms: ts,
        panic_location,
        panic_message,
        backtrace,
    };

    let json = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string());
    fs::write(&path, json)?;
    let _ = prune_old_reports(&dir, MAX_REPORTS);
    Ok(path)
}

fn panic_payload_to_string(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    if let Some(s) = payload.downcast_ref::<&str>() {
        return (*s).to_string();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    "<non-string panic payload>".to_string()
}

pub(crate) fn sanitize_args(raw: Vec<String>) -> Vec<String> {
    raw.into_iter()
        .map(|s| {
            if looks_secret(&s) {
                "<redacted>".to_string()
            } else {
                s
            }
        })
        .collect()
}

fn looks_secret(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    for needle in [
        "token=",
        "secret=",
        "password=",
        "passwd=",
        "api-key=",
        "apikey=",
        "auth=",
        "authorization=",
    ] {
        if lower.contains(needle) {
            return true;
        }
    }
    // Long runs of base64/url-safe chars look like a bearer token.
    if s.len() >= 40
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '/' || c == '+' || c == '='
        })
    {
        return true;
    }
    false
}

pub(crate) fn prune_old_reports(dir: &Path, keep: usize) -> std::io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.ends_with(".json") && e.path().is_file()
        })
        .collect();

    if entries.len() <= keep {
        return Ok(());
    }

    // Sort by mtime ascending so we drop the oldest first.
    entries.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
    let to_remove = entries.len() - keep;
    for entry in entries.into_iter().take(to_remove) {
        let _ = fs::remove_file(entry.path());
    }
    Ok(())
}

/// Scan `crashes_dir` for the newest report newer than the recorded
/// `last_seen` watermark. On a hit, advance the watermark and return
/// `Some((unix_ms, path))` so the caller can surface a one-line notice on
/// stderr. Returns `None` when there's nothing new.
pub(crate) fn surface_previous_report(crashes_dir: &Path) -> Option<(u128, PathBuf)> {
    let last_seen_path = crashes_dir.join(LAST_SEEN_FILE);
    let last_seen: u128 = fs::read_to_string(&last_seen_path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);

    let mut newest: Option<(u128, PathBuf)> = None;
    let entries = fs::read_dir(crashes_dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy().into_owned();
        if !name.ends_with(".json") {
            continue;
        }
        // Filename layout: <unix_ms>-<role>-<pid>.json
        let Some(ms_str) = name.split('-').next() else {
            continue;
        };
        let Ok(ms) = ms_str.parse::<u128>() else {
            continue;
        };
        if ms <= last_seen {
            continue;
        }
        match newest {
            Some((cur, _)) if cur >= ms => {}
            _ => newest = Some((ms, entry.path())),
        }
    }

    if let Some((ms, _)) = &newest {
        if let Ok(mut f) = fs::File::create(&last_seen_path) {
            let _ = writeln!(f, "{ms}");
        }
    }
    newest
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn sanitize_redacts_known_secret_keys_and_long_tokens() {
        let input = vec![
            "clud".to_string(),
            "--repo=foo/bar".to_string(),
            "GH_TOKEN=ghp_abcDEFghiJKLmnoPQRstuVWXyz0123456789".to_string(),
            "secret=hunter2".to_string(),
            "ghp_abcDEFghiJKLmnoPQRstuVWXyz0123456789abcd".to_string(),
            "--flag".to_string(),
            "short".to_string(),
        ];
        let out = sanitize_args(input);
        assert_eq!(out[0], "clud");
        assert_eq!(out[1], "--repo=foo/bar");
        assert_eq!(out[2], "<redacted>", "token= prefix should be redacted");
        assert_eq!(out[3], "<redacted>", "secret= prefix should be redacted");
        assert_eq!(out[4], "<redacted>", "long base64 run should be redacted");
        assert_eq!(out[5], "--flag");
        assert_eq!(out[6], "short", "short non-secret strings pass through");
    }

    #[test]
    fn prune_keeps_only_n_reports() -> std::io::Result<()> {
        let dir = TempDir::new()?;
        // Write 53 dummy reports. We don't care which 50 survive, only the
        // count — mtime-based ordering for "which to drop" is exercised
        // implicitly by `prune_old_reports`'s sort key.
        for i in 0..53 {
            fs::write(
                dir.path().join(format!(
                    "{}-test-{}.json",
                    1_700_000_000_000_u128 + i as u128,
                    i
                )),
                "{}",
            )?;
        }
        prune_old_reports(dir.path(), 50)?;
        let remaining: Vec<_> = fs::read_dir(dir.path())?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
            .collect();
        assert_eq!(remaining.len(), 50, "should keep exactly 50 reports");
        Ok(())
    }

    #[test]
    fn prune_no_op_when_under_cap() -> std::io::Result<()> {
        let dir = TempDir::new()?;
        for i in 0..3 {
            fs::write(dir.path().join(format!("{i}-test-{i}.json")), "{}")?;
        }
        prune_old_reports(dir.path(), 50)?;
        let remaining = fs::read_dir(dir.path())?.count();
        assert_eq!(remaining, 3);
        Ok(())
    }

    #[test]
    fn prune_ignores_non_json_files() -> std::io::Result<()> {
        let dir = TempDir::new()?;
        fs::write(dir.path().join("last_seen"), "0")?;
        for i in 0..52 {
            fs::write(
                dir.path().join(format!(
                    "{}-test-{}.json",
                    1_700_000_000_000_u128 + i as u128,
                    i
                )),
                "{}",
            )?;
        }
        prune_old_reports(dir.path(), 50)?;
        // 50 .json + the last_seen sidecar = 51 entries.
        assert!(
            dir.path().join("last_seen").exists(),
            "last_seen survives prune"
        );
        let jsons = fs::read_dir(dir.path())?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
            .count();
        assert_eq!(jsons, 50);
        Ok(())
    }

    #[test]
    fn surface_returns_newest_and_advances_last_seen() -> std::io::Result<()> {
        let dir = TempDir::new()?;
        fs::write(dir.path().join("100-test-1.json"), "{}")?;
        fs::write(dir.path().join("200-test-2.json"), "{}")?;
        fs::write(dir.path().join("150-test-3.json"), "{}")?;

        let hit = surface_previous_report(dir.path()).expect("expected a newest report");
        assert_eq!(hit.0, 200);
        assert!(hit.1.ends_with("200-test-2.json"));
        // last_seen file should now hold 200, so a re-scan returns None.
        let last_seen = fs::read_to_string(dir.path().join("last_seen"))?;
        assert_eq!(last_seen.trim(), "200");
        assert!(surface_previous_report(dir.path()).is_none());
        Ok(())
    }

    #[test]
    fn surface_returns_none_on_empty_dir() -> std::io::Result<()> {
        let dir = TempDir::new()?;
        assert!(surface_previous_report(dir.path()).is_none());
        assert!(!dir.path().join("last_seen").exists());
        Ok(())
    }
}
