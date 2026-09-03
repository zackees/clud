use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::types::ENV_BACKLOG_BYTES;

pub(super) fn child_env() -> Vec<(String, String)> {
    let originator_key = running_process::ORIGINATOR_ENV_VAR;
    // Issue #509: session temp redirect. Strip any inherited TMPDIR/TMP/TEMP
    // so the override below isn't shadowed by a stale value carried in from
    // the parent env.
    let overrides = crate::gc::session_tmp::env_overrides();
    let strip_temp = !overrides.is_empty();
    // Issue #753: same Git-Bash completion suppression the foreground runner
    // applies (see shell::completion_guard). This builder is a deliberate
    // duplicate of `runner::child_env` for the daemon-launched path — any
    // policy added to one belongs in both, or daemon sessions silently miss
    // it.
    let completion = crate::shell::completion_guard::env_overrides();
    // Issue #1066: arm `set -u` in every non-interactive bash the backend
    // spawns. Wired here as well as in `runner::child_env` for the reason the
    // comment above gives — a policy in one and not the other means daemon
    // sessions silently miss it, and a safety default that is only sometimes
    // present is the worst of both.
    let nounset = crate::shell::nounset::env_overrides();
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(key, _)| key != "IN_CLUD" && key != originator_key)
        .filter(|(key, _)| {
            !strip_temp || !crate::gc::session_tmp::OVERRIDDEN_KEYS.contains(&key.as_str())
        })
        // Strip any inherited value so the override below is the only one.
        .filter(|(key, _)| !completion.iter().any(|(k, _)| k == key))
        // Strip only the keys nounset is actually replacing — the same shape
        // as the line above, and load-bearing for the same reason. Filtering a
        // fixed key list instead would delete a user's inherited BASH_ENV in
        // the cases where `env_overrides` returns nothing (opted out, no home,
        // unwritable state dir), since nothing would then put it back. The
        // runner's `push_or_replace` is inert when the overrides are empty;
        // this has to be too, or opting out means something different
        // depending on which builder launched you.
        .filter(|(key, _)| !nounset.iter().any(|(k, _)| k == key))
        .collect();
    env.push(("IN_CLUD".to_string(), "1".to_string()));
    env.push((
        originator_key.to_string(),
        format!("CLUD:{}", std::process::id()),
    ));
    env.extend(overrides);
    env.extend(completion);
    env.extend(nounset);
    env
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

/// Mint a session id.
///
/// Daemon-side and unique per call, which is load-bearing beyond naming
/// (#305). The spawn-storm that issue describes needs two `Create` requests
/// to race on *the same* session, and nothing can arrange that: the id is
/// minted here rather than supplied by the client — `WorkerLaunchSpec` has no
/// id field to carry one — so per-session spawn serialization would lock a
/// key that is unique by construction. `new_session_ids_never_collide` pins
/// that; if a client-supplied id is ever added, revisit #305 phase 2.
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

    /// Issue #753: the daemon builds its own child env and historically
    /// drifted from `runner::child_env`. Assert the completion suppression
    /// reaches daemon-launched sessions too, exactly once.
    #[cfg(windows)]
    #[test]
    fn child_env_suppresses_git_bash_completions() {
        use crate::shell::completion_guard::{OPT_OUT_KEY, SUPPRESS_KEY};

        let guard = EnvGuard::unset(OPT_OUT_KEY);
        let env = child_env();
        drop(guard);

        let hits: Vec<_> = env.iter().filter(|(k, _)| k == SUPPRESS_KEY).collect();
        assert_eq!(
            hits.len(),
            1,
            "{SUPPRESS_KEY} must appear exactly once in the daemon child env"
        );
        assert_eq!(hits[0].1, "1");
    }

    /// The opt-out must reach the daemon path as well, and must not leave a
    /// stale inherited value behind.
    #[cfg(windows)]
    #[test]
    fn child_env_opt_out_drops_suppression() {
        use crate::shell::completion_guard::{OPT_OUT_KEY, SUPPRESS_KEY};

        let opt_out = EnvGuard::set(OPT_OUT_KEY, "1");
        let env = child_env();
        drop(opt_out);

        // Inherited ambient value (if any) is all that may remain; we must not
        // have injected one ourselves.
        let injected =
            std::env::var(SUPPRESS_KEY).is_err() && env.iter().any(|(k, _)| k == SUPPRESS_KEY);
        assert!(!injected, "{OPT_OUT_KEY}=1 must suppress the injection");
    }

    /// Issue #1066, same drift risk as the #753 pair above and the reason the
    /// comment in `child_env` says a policy belongs in both builders: assert
    /// the daemon path arms nounset identically to `runner::child_env`,
    /// rather than assuming the two edits stayed in step.
    #[test]
    fn child_env_arms_nounset_exactly_like_the_runner() {
        use crate::shell::nounset::{BASH_ENV_KEY, OPT_OUT_KEY};

        let guard = EnvGuard::set_all(&[(OPT_OUT_KEY, None), (BASH_ENV_KEY, None)]);
        let daemon = child_env();
        let runner = crate::runner::child_env();
        drop(guard);

        let pick = |env: &[(String, String)]| -> Vec<String> {
            env.iter()
                .filter(|(key, _)| key == BASH_ENV_KEY)
                .map(|(_, value)| value.clone())
                .collect()
        };
        let (daemon, runner) = (pick(&daemon), pick(&runner));
        assert_eq!(
            daemon.len(),
            1,
            "{BASH_ENV_KEY} must appear exactly once in the daemon child env"
        );
        assert_eq!(
            daemon, runner,
            "the two builders must arm nounset the same way; they have drifted"
        );
    }

    /// Regression: the daemon builder used to strip a fixed key list before
    /// layering, so when the overrides came back empty — opted out, no home,
    /// unwritable state dir — nothing put the user's inherited `BASH_ENV`
    /// back, and opting out *deleted* their startup file instead of leaving
    /// the shell alone. The runner path, which layers with `push_or_replace`,
    /// never had the bug, which is precisely what made it invisible.
    #[test]
    fn opting_out_preserves_an_inherited_bash_env_in_both_builders() {
        use crate::shell::nounset::{BASH_ENV_KEY, OPT_OUT_KEY};

        let theirs = "/home/someone/their-bash-env.sh";
        let guard = EnvGuard::set_all(&[(OPT_OUT_KEY, Some("1")), (BASH_ENV_KEY, Some(theirs))]);
        let daemon = child_env();
        let runner = crate::runner::child_env();
        drop(guard);

        for (label, env) in [("daemon", &daemon), ("runner", &runner)] {
            let got: Vec<&str> = env
                .iter()
                .filter(|(key, _)| key == BASH_ENV_KEY)
                .map(|(_, value)| value.as_str())
                .collect();
            assert_eq!(
                got,
                vec![theirs],
                "{label}: opting out must leave the user's {BASH_ENV_KEY} untouched"
            );
        }
    }

    /// #305 phase 2 proposes a per-session spawn mutex so concurrent
    /// `Create`s for one session cannot race. That collision is impossible
    /// while ids are minted per request, so this pins the property the
    /// conclusion rests on rather than the conclusion.
    ///
    /// Concurrent on purpose: a counter that looked unique single-threaded
    /// but handed the same value to two threads is exactly the failure that
    /// would resurrect the phase.
    #[test]
    fn new_session_ids_never_collide() {
        use std::collections::HashSet;

        const THREADS: usize = 8;
        const PER_THREAD: usize = 256;

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                std::thread::spawn(|| {
                    (0..PER_THREAD)
                        .map(|_| new_session_id())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let ids: Vec<String> = handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("id thread"))
            .collect();

        let unique: HashSet<&String> = ids.iter().collect();
        assert_eq!(
            unique.len(),
            THREADS * PER_THREAD,
            "session ids must be unique; a collision would make #305's \
             per-session spawn race reachable"
        );
    }

    /// The ids carry the millisecond they were minted in, so two created in
    /// the same millisecond still differ. Without the counter they would not,
    /// and a fast enough pair of launches would collide.
    #[test]
    fn ids_minted_in_the_same_millisecond_still_differ() {
        let first = new_session_id();
        let second = new_session_id();
        assert_ne!(first, second);
        assert!(first.starts_with("sess-"), "{first}");
    }

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

        /// Several keys under one lock. `EnvGuard`'s mutex is not reentrant,
        /// so holding two single-key guards at once deadlocks — and the
        /// nounset cases below need the opt-out and an inherited `BASH_ENV`
        /// set together.
        fn set_all(vars: &[(&'static str, Option<&str>)]) -> MultiEnvGuard {
            let lock = Self::lock();
            let prior = vars
                .iter()
                .map(|(key, value)| {
                    let prior = std::env::var(*key).ok();
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                    (*key, prior)
                })
                .collect();
            MultiEnvGuard { prior, _lock: lock }
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

    struct MultiEnvGuard {
        prior: Vec<(&'static str, Option<String>)>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for MultiEnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.prior.drain(..) {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}
