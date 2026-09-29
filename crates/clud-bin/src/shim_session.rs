//! Session-launch env for the shim aliases of the multicall `clud`.
//!
//! [`activate_rm`] is the one child-env layer that installs the session
//! aliases, puts their directory first on PATH, and exports the session keys
//! the shims validate. The key names and alias list come from
//! [`crate::shim_registry`]; the out-of-session contract (fail open to the
//! next real binary) is in `docs/architecture/shim-dispatch.md`.

use std::path::Path;

use crate::shim_registry;

/// Env var name for `PATH`. Same on Unix and Windows (Windows is
/// case-insensitive but uppercase is conventional).
pub const PATH_ENV_VAR: &str = "PATH";
/// Read by the generated BASH_ENV file after a login shell resets PATH.
pub const RM_SHIM_DIR_KEY: &str = shim_registry::SESSION_DIR_KEY;
pub const GH_SHIM_TARGET_KEY: &str = shim_registry::GH_TARGET_KEY;
pub const GH_SHIM_ACTIVE_KEY: &str = shim_registry::GH_ACTIVE_KEY;
pub const GH_SHIM_FAIL_FAST_KEY: &str = shim_registry::GH_FAIL_FAST_KEY;
/// Git's terminal-prompt switch. An agent cannot answer a terminal prompt, so
/// a credential failure must error instead of waiting (#1546).
pub const GIT_TERMINAL_PROMPT_KEY: &str = "GIT_TERMINAL_PROMPT";
/// Set to empty when the caller did not choose one. Git then skips askpass
/// instead of falling back to `SSH_ASKPASS`, whose GUI dialog blocked agent
/// pushes when the credential helper failed (#1546). `SSH_ASKPASS` itself is
/// left alone for ssh's own key unlock.
pub const GIT_ASKPASS_KEY: &str = "GIT_ASKPASS";

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
    set_env(env, GIT_TERMINAL_PROMPT_KEY, "0");
    if !env
        .iter()
        .any(|(key, _)| key.eq_ignore_ascii_case(GIT_ASKPASS_KEY))
    {
        set_env(env, GIT_ASKPASS_KEY, "");
    }
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
            let home = env
                .iter()
                .find(|(k, _)| k == home_key)
                .map(|(_, home)| std::path::PathBuf::from(home));
            let target = resolve_gh_target(&original_path, inherited_target, &dir, home.as_deref());
            if let Some(target) = target {
                let target = target.canonicalize().unwrap_or(target);
                set_env(env, GH_SHIM_TARGET_KEY, &target.to_string_lossy());
                set_env(env, GH_SHIM_ACTIVE_KEY, "1");
            } else {
                set_env(env, GH_SHIM_ACTIVE_KEY, "0");
            }
            prepend_to_path(env, &dir);
            set_env(env, RM_SHIM_DIR_KEY, &dir.to_string_lossy());
            set_env(env, shim_registry::ABI_KEY, shim_registry::SHIM_ABI);
        }
        Err(error) => {
            eprintln!("[clud rm shim] installation failed; shell identity guard will deny: {error}")
        }
    }
}

/// The real `gh`: an inherited target that is still valid, else the first
/// `gh` on the PATH the session had before the alias directory was
/// prepended. Copies of the packaged shim and every shim directory are
/// skipped, so a nested launch cannot select an alias.
fn resolve_gh_target(
    original_path: &str,
    inherited_target: Option<std::path::PathBuf>,
    shim_dir: &Path,
    home: Option<&Path>,
) -> Option<std::path::PathBuf> {
    let shim = crate::shim_install::packaged_shim().unwrap_or_else(|_| shim_dir.join("gh"));
    let mut dirs = shim_registry::shim_dirs(&shim, Some(shim_dir), home);
    // The packaged shim's own directory holds `clud` itself, not aliases.
    if let Some(own) = std::fs::canonicalize(&shim)
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    {
        dirs.retain(|dir| *dir != own);
    }
    inherited_target
        .filter(|path| shim_registry::valid_target(path, &shim, &dirs))
        .or_else(|| {
            shim_registry::first_on_path(
                &shim_registry::file_name("gh"),
                std::ffi::OsStr::new(original_path),
                &shim,
                &dirs,
            )
        })
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
            resolve_gh_target(&path, None, &shim_dir, None),
            Some(real.clone())
        );
        assert_eq!(
            resolve_gh_target(&path, Some(real.clone()), &shim_dir, None),
            Some(real.clone())
        );
        assert_eq!(
            resolve_gh_target(&path, Some(shim_dir.join(name)), &shim_dir, None),
            Some(real),
            "an inherited target inside the alias directory is re-resolved"
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
        let mut e = env(&[("CLUD_EXAMPLE_KEY", "/old/sock")]);
        set_env(&mut e, "CLUD_EXAMPLE_KEY", "/new/sock");
        let v = &e.iter().find(|(k, _)| k == "CLUD_EXAMPLE_KEY").unwrap().1;
        assert_eq!(v, "/new/sock");
    }

    #[test]
    fn set_env_creates_when_absent() {
        let mut e = env(&[("OTHER", "x")]);
        set_env(&mut e, "CLUD_EXAMPLE_KEY", "/sock");
        assert!(e
            .iter()
            .any(|(k, v)| k == "CLUD_EXAMPLE_KEY" && v == "/sock"));
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
