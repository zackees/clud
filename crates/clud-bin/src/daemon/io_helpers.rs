use std::fs;
use std::io::{self, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::types::ENV_BACKLOG_BYTES;

pub(super) fn child_env() -> Vec<(String, String)> {
    let originator_key = running_process_core::ORIGINATOR_ENV_VAR;
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(key, _)| key != "IN_CLUD" && key != originator_key)
        .collect();
    env.push(("IN_CLUD".to_string(), "1".to_string()));
    env.push((
        originator_key.to_string(),
        format!("CLUD:{}", std::process::id()),
    ));
    env
}

pub(super) fn write_json_line<T: Serialize>(writer: &mut TcpStream, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

pub(super) fn write_json_file<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing parent"))?;
    fs::create_dir_all(parent)?;
    let temp_path = path.with_extension("tmp");
    fs::write(
        &temp_path,
        serde_json::to_vec_pretty(value)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?,
    )?;
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    fs::rename(temp_path, path)
}

pub(super) fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<T> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

pub(super) fn new_session_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let sequence = COUNTER.fetch_add(1, Ordering::AcqRel);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("sess-{millis}-{sequence}")
}

pub(super) fn terminal_dimensions() -> (u16, u16) {
    if let Some((width, height)) = terminal_size::terminal_size() {
        (height.0, width.0)
    } else {
        (24, 32767)
    }
}

/// Resolve the attach-replay backlog cap in bytes. Precedence: explicit CLI
/// flag (`--backlog-size`) > `CLUD_BACKLOG_BYTES` env var > compiled default.
/// Returns `None` when no override was set, so the worker spec stays
/// wire-compatible with older daemons.
pub(super) fn resolve_backlog_bytes(cli: Option<&str>) -> Option<usize> {
    if let Some(raw) = cli {
        return parse_byte_size(raw);
    }
    if let Ok(raw) = std::env::var(ENV_BACKLOG_BYTES) {
        return parse_byte_size(&raw);
    }
    None
}

/// Parse a human-friendly byte count: `256`, `256k`, `1mb`, `2MiB`, etc.
/// Returns `None` when the input is unparseable or non-positive so we fall
/// back to the compiled default instead of misconfiguring the cap.
pub(super) fn parse_byte_size(raw: &str) -> Option<usize> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let (num_part, mult) = if let Some(rest) = lower
        .strip_suffix("kib")
        .or_else(|| lower.strip_suffix("kb"))
        .or_else(|| lower.strip_suffix("k"))
    {
        (rest, 1024usize)
    } else if let Some(rest) = lower
        .strip_suffix("mib")
        .or_else(|| lower.strip_suffix("mb"))
        .or_else(|| lower.strip_suffix("m"))
    {
        (rest, 1024 * 1024)
    } else if let Some(rest) = lower
        .strip_suffix("gib")
        .or_else(|| lower.strip_suffix("gb"))
        .or_else(|| lower.strip_suffix("g"))
    {
        (rest, 1024 * 1024 * 1024)
    } else if let Some(rest) = lower.strip_suffix("b") {
        (rest, 1usize)
    } else {
        (lower.as_str(), 1usize)
    };
    let n: usize = num_part.trim().parse().ok()?;
    if n == 0 {
        return None;
    }
    n.checked_mul(mult)
}

#[cfg(test)]
mod tests {
    //! Issue #25: configurable attach-replay backlog cap.
    use super::*;

    #[test]
    fn parse_byte_size_raw_bytes() {
        assert_eq!(parse_byte_size("262144"), Some(262144));
        assert_eq!(parse_byte_size("1024b"), Some(1024));
        assert_eq!(parse_byte_size("  2048  "), Some(2048));
    }

    #[test]
    fn parse_byte_size_with_kb_suffix() {
        assert_eq!(parse_byte_size("256k"), Some(256 * 1024));
        assert_eq!(parse_byte_size("256kb"), Some(256 * 1024));
        assert_eq!(parse_byte_size("256KiB"), Some(256 * 1024));
        assert_eq!(parse_byte_size("256KB"), Some(256 * 1024));
    }

    #[test]
    fn parse_byte_size_with_mb_suffix() {
        assert_eq!(parse_byte_size("1m"), Some(1024 * 1024));
        assert_eq!(parse_byte_size("1MB"), Some(1024 * 1024));
        assert_eq!(parse_byte_size("1MiB"), Some(1024 * 1024));
        assert_eq!(parse_byte_size("2MB"), Some(2 * 1024 * 1024));
    }

    #[test]
    fn parse_byte_size_with_gb_suffix() {
        assert_eq!(parse_byte_size("1g"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_byte_size("1gib"), Some(1024 * 1024 * 1024));
    }

    #[test]
    fn parse_byte_size_rejects_garbage() {
        assert_eq!(parse_byte_size(""), None);
        assert_eq!(parse_byte_size("abc"), None);
        assert_eq!(parse_byte_size("0"), None);
        assert_eq!(parse_byte_size("0k"), None);
        assert_eq!(parse_byte_size("-5"), None);
    }

    #[test]
    fn resolve_backlog_bytes_prefers_cli_over_env() {
        let guard = EnvGuard::set(ENV_BACKLOG_BYTES, "2mb");
        assert_eq!(resolve_backlog_bytes(Some("128k")), Some(128 * 1024));
        drop(guard);
    }

    #[test]
    fn resolve_backlog_bytes_falls_back_to_env() {
        let guard = EnvGuard::set(ENV_BACKLOG_BYTES, "512k");
        assert_eq!(resolve_backlog_bytes(None), Some(512 * 1024));
        drop(guard);
    }

    #[test]
    fn resolve_backlog_bytes_none_when_unset() {
        let guard = EnvGuard::unset(ENV_BACKLOG_BYTES);
        assert_eq!(resolve_backlog_bytes(None), None);
        drop(guard);
    }

    /// RAII env-var guard so tests that read `CLUD_BACKLOG_BYTES` don't
    /// contaminate each other or the outer process. Serial by mutex since
    /// `std::env` is process-global.
    struct EnvGuard {
        key: &'static str,
        prior: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn lock() -> std::sync::MutexGuard<'static, ()> {
            static M: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
            M.get_or_init(|| std::sync::Mutex::new(()))
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
        }

        fn set(key: &'static str, value: &str) -> Self {
            let lock = Self::lock();
            let prior = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self {
                key,
                prior,
                _lock: lock,
            }
        }

        fn unset(key: &'static str) -> Self {
            let lock = Self::lock();
            let prior = std::env::var(key).ok();
            std::env::remove_var(key);
            Self {
                key,
                prior,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prior.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}
