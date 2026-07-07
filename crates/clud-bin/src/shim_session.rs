//! Session-launch env injection for the Python shim. Slice 5 of #406 /
//! #413.
//!
//! When clud spawns a backend / agent / tool child, this module's
//! [`inject_shim_env`] mutates the child's env vec in-place to:
//!
//! 1. Prepend `~/.clud/state/shims/` to `PATH` so `python` /
//!    `python3` resolve to the shim binaries the slice 4 (#412)
//!    installer materialized there.
//! 2. Set `CLUD_DAEMON_SOCKET=<path>` so the shim can connect back to
//!    the daemon and call the slice 2 (#410) `ResolveInterpreter`
//!    handler.
//!
//! Existing env vars are preserved; only `PATH` is rewritten and only
//! `CLUD_DAEMON_SOCKET` is added (or replaced if a prior set existed).
//! Slice 5 is conservative — no other env mutation. Skipping injection
//! is the deliberate fallback when the shims dir or daemon socket
//! isn't available (CI / minimal containers).

use std::path::Path;

/// Env var the shim reads to find the daemon socket. Mirrors the
/// constant defined in `clud_shim.rs` so the two stay in sync if one
/// is renamed.
pub const SHIM_DAEMON_SOCKET_VAR: &str = "CLUD_DAEMON_SOCKET";

/// Env var name for `PATH`. Same on Unix and Windows (Windows is
/// case-insensitive but uppercase is conventional).
pub const PATH_ENV_VAR: &str = "PATH";

/// Mutate `env` in place: prepend `shims_dir` to PATH and set
/// `CLUD_DAEMON_SOCKET` to `daemon_socket`. Returns `(path_prepended,
/// socket_set)` so callers can log what actually changed.
///
/// Pass `None` for either argument to skip that part of the injection
/// — useful when one or the other isn't available in the current
/// session.
pub fn inject_shim_env(
    env: &mut Vec<(String, String)>,
    shims_dir: Option<&Path>,
    daemon_socket: Option<&str>,
) -> (bool, bool) {
    let path_prepended = if let Some(dir) = shims_dir {
        prepend_to_path(env, dir)
    } else {
        false
    };
    let socket_set = if let Some(sock) = daemon_socket {
        set_env(env, SHIM_DAEMON_SOCKET_VAR, sock);
        true
    } else {
        false
    };
    (path_prepended, socket_set)
}

/// Prepend `dir` to the PATH entry in `env` (or create a new entry if
/// PATH isn't set). Idempotent: re-injection doesn't duplicate the
/// prepended dir.
pub fn prepend_to_path(env: &mut Vec<(String, String)>, dir: &Path) -> bool {
    let dir_str = dir.to_string_lossy().into_owned();
    let sep = path_sep();
    for (key, value) in env.iter_mut() {
        if key.eq_ignore_ascii_case(PATH_ENV_VAR) {
            if value.split(sep).any(|p| p == dir_str) {
                return false; // already present
            }
            let new = if value.is_empty() {
                dir_str.clone()
            } else {
                format!("{dir_str}{sep}{value}")
            };
            *value = new;
            return true;
        }
    }
    // PATH not in env at all — create it.
    env.push((PATH_ENV_VAR.to_string(), dir_str));
    true
}

/// Set `key=value` in `env`, replacing any existing entry with the
/// same key (case-insensitive match — Windows env vars are
/// case-insensitive and clud aims to behave consistently).
pub fn set_env(env: &mut Vec<(String, String)>, key: &str, value: &str) {
    for (k, v) in env.iter_mut() {
        if k.eq_ignore_ascii_case(key) {
            *v = value.to_string();
            return;
        }
    }
    env.push((key.to_string(), value.to_string()));
}

fn path_sep() -> char {
    #[cfg(windows)]
    {
        ';'
    }
    #[cfg(not(windows))]
    {
        ':'
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn prepend_to_path_when_path_already_present() {
        let mut e = env(&[("PATH", "/usr/bin:/bin")]);
        let added = prepend_to_path(&mut e, Path::new("/home/u/.clud/state/shims"));
        assert!(added);
        let path = &e.iter().find(|(k, _)| k == "PATH").unwrap().1;
        assert!(path.starts_with("/home/u/.clud/state/shims"), "got {path}");
        assert!(path.contains("/usr/bin"));
    }

    #[test]
    fn prepend_to_path_is_idempotent() {
        let initial_path = format!("/home/u/.clud/state/shims{}/usr/bin", path_sep());
        let mut e = env(&[("PATH", initial_path.as_str())]);
        let added = prepend_to_path(&mut e, Path::new("/home/u/.clud/state/shims"));
        assert!(!added, "second prepend should be a no-op");
        // Path unchanged.
        let path = &e.iter().find(|(k, _)| k == "PATH").unwrap().1;
        assert_eq!(path, &initial_path);
    }

    #[test]
    fn prepend_to_path_creates_path_when_absent() {
        let mut e = env(&[("OTHER", "x")]);
        let added = prepend_to_path(&mut e, Path::new("/shims"));
        assert!(added);
        let path = &e.iter().find(|(k, _)| k == "PATH").unwrap().1;
        assert_eq!(path, "/shims");
    }

    #[test]
    fn set_env_replaces_existing_value() {
        let mut e = env(&[("CLUD_DAEMON_SOCKET", "/old/sock")]);
        set_env(&mut e, "CLUD_DAEMON_SOCKET", "/new/sock");
        let v = &e.iter().find(|(k, _)| k == "CLUD_DAEMON_SOCKET").unwrap().1;
        assert_eq!(v, "/new/sock");
    }

    #[test]
    fn set_env_creates_when_absent() {
        let mut e = env(&[("OTHER", "x")]);
        set_env(&mut e, "CLUD_DAEMON_SOCKET", "/sock");
        assert!(e
            .iter()
            .any(|(k, v)| k == "CLUD_DAEMON_SOCKET" && v == "/sock"));
    }

    #[test]
    fn inject_shim_env_does_both_paths() {
        let mut e = env(&[("PATH", "/usr/bin")]);
        let (path_done, socket_done) =
            inject_shim_env(&mut e, Some(Path::new("/shims")), Some("/daemon.sock"));
        assert!(path_done);
        assert!(socket_done);
        let path = &e.iter().find(|(k, _)| k == "PATH").unwrap().1;
        assert!(path.starts_with("/shims"));
        let sock = &e.iter().find(|(k, _)| k == "CLUD_DAEMON_SOCKET").unwrap().1;
        assert_eq!(sock, "/daemon.sock");
    }

    #[test]
    fn inject_shim_env_skips_when_none() {
        let mut e = env(&[("PATH", "/usr/bin")]);
        let (path_done, socket_done) = inject_shim_env(&mut e, None, None);
        assert!(!path_done);
        assert!(!socket_done);
        // PATH unchanged, no CLUD_DAEMON_SOCKET added.
        assert_eq!(
            e.iter().find(|(k, _)| k == "PATH").unwrap().1,
            "/usr/bin".to_string()
        );
        assert!(e.iter().all(|(k, _)| k != "CLUD_DAEMON_SOCKET"));
    }

    #[test]
    fn inject_shim_env_partial_path_only() {
        let mut e = env(&[("PATH", "/usr/bin")]);
        let (path_done, socket_done) = inject_shim_env(&mut e, Some(Path::new("/shims")), None);
        assert!(path_done);
        assert!(!socket_done);
        assert!(e.iter().all(|(k, _)| k != "CLUD_DAEMON_SOCKET"));
    }

    #[test]
    fn inject_shim_env_partial_socket_only() {
        let mut e = env(&[("PATH", "/usr/bin")]);
        let (path_done, socket_done) = inject_shim_env(&mut e, None, Some("/sock"));
        assert!(!path_done);
        assert!(socket_done);
        // PATH untouched.
        assert_eq!(
            e.iter().find(|(k, _)| k == "PATH").unwrap().1,
            "/usr/bin".to_string()
        );
    }

    #[test]
    fn path_handling_uses_platform_separator() {
        let mut e = env(&[("PATH", "/usr/bin")]);
        prepend_to_path(&mut e, &PathBuf::from("/shims"));
        let path = &e.iter().find(|(k, _)| k == "PATH").unwrap().1;
        let sep = path_sep();
        assert!(
            path.contains(sep),
            "PATH {path} must contain platform sep {sep}"
        );
    }
}
