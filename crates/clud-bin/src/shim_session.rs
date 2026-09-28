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
/// Read by the generated BASH_ENV file after a login shell resets PATH.
pub const RM_SHIM_DIR_KEY: &str = "CLUD_RM_SHIM_DIR";
pub const GH_SHIM_TARGET_KEY: &str = "CLUD_GH_SHIM_TARGET";
pub const GH_SHIM_ACTIVE_KEY: &str = "CLUD_GH_SHIM_ACTIVE";
pub const GH_SHIM_FAIL_FAST_KEY: &str = "CLUD_GH_SHIM_FAIL_FAST";

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
            if value.split(sep).next() == Some(dir_str.as_str()) {
                return false; // already present
            }
            *value = value
                .split(sep)
                .filter(|p| *p != dir_str)
                .collect::<Vec<_>>()
                .join(&sep.to_string());
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

/// Both foreground and daemon call this after assembling the effective child env.
/// Installation errors remain visible and the mandatory hook identity check denies.
pub fn activate_rm(env: &mut Vec<(String, String)>) {
    let fail_fast = crate::clud_settings::load_pr_wait_fail_fast_enabled().unwrap_or(true);
    set_env(
        env,
        GH_SHIM_FAIL_FAST_KEY,
        if fail_fast { "1" } else { "0" },
    );
    set_env(env, GH_SHIM_ACTIVE_KEY, "0");
    let original_path = env
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(PATH_ENV_VAR))
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    let inherited_target = env
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(GH_SHIM_TARGET_KEY))
        .map(|(_, value)| std::path::PathBuf::from(value));
    let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let result = env
        .iter()
        .find(|(k, _)| k == home_key)
        .ok_or_else(|| std::io::Error::other("missing session home"))
        .and_then(|(_, home)| {
            crate::shim_install::packaged_shim()
                .and_then(|source| crate::shim_install::install_rm_at(Path::new(home), &source))
        });
    match result {
        Ok(dir) => {
            let target = resolve_gh_target(&original_path, inherited_target, &dir);
            if let Some(target) = target {
                let target = target.canonicalize().unwrap_or(target);
                set_env(env, GH_SHIM_TARGET_KEY, &target.to_string_lossy());
                set_env(env, GH_SHIM_ACTIVE_KEY, "1");
            } else {
                set_env(env, GH_SHIM_ACTIVE_KEY, "0");
            }
            prepend_to_path(env, &dir);
            set_env(env, RM_SHIM_DIR_KEY, &dir.to_string_lossy());
        }
        Err(error) => {
            eprintln!("[clud rm shim] installation failed; shell identity guard will deny: {error}")
        }
    }
}

fn resolve_gh_target(
    original_path: &str,
    inherited_target: Option<std::path::PathBuf>,
    shim_dir: &Path,
) -> Option<std::path::PathBuf> {
    inherited_target
        .filter(|path| path.is_absolute() && executable(path) && !path.starts_with(shim_dir))
        .or_else(|| {
            std::env::split_paths(original_path)
                .filter(|path| path != shim_dir)
                .map(|path| path.join(if cfg!(windows) { "gh.exe" } else { "gh" }))
                .find(|path| executable(path))
        })
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn gh_target_resolution_skips_the_alias_before_path_prepend() {
        let temp = TempDir::new().unwrap();
        let shim_dir = temp.path().join("shim");
        let real_dir = temp.path().join("real");
        std::fs::create_dir_all(&shim_dir).unwrap();
        std::fs::create_dir_all(&real_dir).unwrap();
        let name = if cfg!(windows) { "gh.exe" } else { "gh" };
        std::fs::write(shim_dir.join(name), b"shim").unwrap();
        let real = real_dir.join(name);
        std::fs::write(&real, b"real").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = std::env::join_paths([&shim_dir, &real_dir]).unwrap();
        let path = path.to_string_lossy();
        assert_eq!(
            resolve_gh_target(&path, None, &shim_dir),
            Some(real.clone())
        );
        assert_eq!(
            resolve_gh_target(&path, Some(real.clone()), &shim_dir),
            Some(real)
        );
    }

    #[test]
    fn moves_existing_later_entry_to_front() {
        let sep = path_sep();
        let mut e = env(&[("PATH", &format!("/system{sep}/shims{sep}/last"))]);
        assert!(prepend_to_path(&mut e, Path::new("/shims")));
        assert_eq!(e[0].1, format!("/shims{sep}/system{sep}/last"));
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
