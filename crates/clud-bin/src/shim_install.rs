//! Per-user shim extraction. Slice 4 of #406 / #412.
//!
//! Materializes `clud` itself under `~/.clud/state/shims/` as the alias names
//! downstream tooling will invoke (`python`, `python3`, `python.exe`,
//! `python3.exe`, `rm`, ...). `clud` is multicall (#1551): each alias is a
//! hardlink, symlink or copy of the running `clud` (see
//! [`crate::alias_link`]) that [`crate::multicall`] routes to the shim by
//! argv[0]. The shim dir is **per-user**, shared across all sessions and
//! concurrent clud processes for that user; an upgrade replaces `clud`, and
//! the next launch relinks every alias that no longer resolves to it.
//!
//! Startup resolves the real Python before rewriting PATH and exports that
//! target for the shim to execute without recursive lookup.

use std::path::{Path, PathBuf};

use crate::alias_link;
use crate::shim_registry::{self, ShimDir, ShimKind};

/// Subdirectory under the user's home where the interpreter aliases live.
/// Joined to the resolved home by [`shims_dir`].
pub const SHIMS_SUBDIR: &str = shim_registry::INTERPRETER_SUBDIR;

/// Alias filenames to install under [`SHIMS_SUBDIR`], derived from
/// [`shim_registry::SHIMS`]. Each alias is a link to (or copy of) `clud`,
/// which dispatches on argv\[0\].
pub fn alias_names() -> Vec<String> {
    shim_registry::file_names_in(ShimDir::Interpreter)
}

/// `~/.clud/state/shims/`. Returns `None` when the user's home dir
/// cannot be resolved — callers degrade silently (the launch path
/// just skips shim install).
pub fn shims_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join(SHIMS_SUBDIR))
}

/// Testable variant — install all aliases under `home_root` as links to
/// `shim_source` (the running `clud`). Returns the count of aliases created
/// or refreshed.
///
/// If `shim_source` does not exist, returns 0 — the session can still
/// proceed; the agent's `python` invocation just won't route through the
/// shim. This is the deliberate graceful-fallback behavior the no-daemon case
/// relies on. A failed hardlink or symlink falls back to a copy; only a
/// failed copy is an error.
pub fn extract_shims_at(home_root: &Path, shim_source: &Path) -> std::io::Result<usize> {
    let shims_dir = home_root.join(SHIMS_SUBDIR);
    std::fs::create_dir_all(&shims_dir)?;
    if !shim_source.is_file() {
        return Ok(0);
    }
    // The pre-#1551 byte-hash sentinel; freshness is now file identity.
    let _ = std::fs::remove_file(shims_dir.join(".shim-hash"));
    let mut installed = 0;
    for alias in alias_names() {
        let target = shims_dir.join(&alias);
        if let alias_link::Placed::Made(_) = alias_link::install_alias(shim_source, &target)? {
            installed += 1;
        }
    }
    Ok(installed)
}

/// Install the interpreter aliases and make them authoritative for one child
/// environment. The real interpreter is resolved before PATH is rewritten so
/// the shim can never select itself recursively.
pub fn prepare_session_shims_at(
    home_root: &Path,
    current_exe: &Path,
    env: &mut Vec<(String, String)>,
) -> std::io::Result<bool> {
    if !is_clud_exe(current_exe) || !current_exe.is_file() {
        return Ok(false);
    }
    let source = current_exe;
    let inherited_target = env
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(shim_registry::PYTHON_TARGET_KEY))
        .map(|(_, value)| PathBuf::from(value))
        .filter(|path| path.is_file());
    let target = match inherited_target {
        Some(path) => path,
        None => {
            let path = env
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("PATH"))
                .map(|(_, value)| value.as_str())
                .unwrap_or("");
            let python = shim_registry::SHIMS
                .iter()
                .find(|spec| spec.kind == ShimKind::Python)
                .map_or_else(String::new, |spec| spec.name.to_string());
            let Some(path) = crate::shim_resolve::which_python_default(&python, path) else {
                return Ok(false);
            };
            path
        }
    };

    extract_shims_at(home_root, source)?;
    let installed = home_root.join(SHIMS_SUBDIR);
    crate::shim_session::prepend_to_path(env, &installed);
    crate::shim_session::set_env(
        env,
        shim_registry::PYTHON_TARGET_KEY,
        &target.to_string_lossy(),
    );
    crate::shim_session::set_env(env, shim_registry::ABI_KEY, shim_registry::SHIM_ABI);
    Ok(true)
}

/// Best-effort startup wiring for the current process. Called after the Ctrl+C
/// handler is installed so a cold-HOME shim extract cannot delay SIGINT
/// delivery, but before the backend is spawned so every launch path inherits
/// the same aliases.
pub fn prepare_current_session() -> std::io::Result<bool> {
    let Some(home) = home_dir() else {
        return Ok(false);
    };
    let current_exe = std::env::current_exe()?;
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    if !prepare_session_shims_at(&home, &current_exe, &mut env)? {
        return Ok(false);
    }
    for key in [
        "PATH",
        shim_registry::PYTHON_TARGET_KEY,
        shim_registry::ABI_KEY,
    ] {
        if let Some((_, value)) = env.iter().find(|(candidate, _)| candidate == key) {
            // SAFETY: startup-only write to the shim session keys, before the backend
            // is spawned and before any thread that reads the environment runs.
            unsafe { std::env::set_var(key, value) };
        }
    }
    Ok(true)
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

/// Whether `exe` is `clud` itself (any case, `.exe` or not) and so a valid
/// alias source. A test harness or another program is never one.
fn is_clud_exe(exe: &Path) -> bool {
    shim_registry::invoked_name(exe.as_os_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("clud"))
}

/// The running `clud`: the trusted alias source, never resolved through PATH
/// or an env override. Errors when the process is not `clud` (a unit-test
/// harness, say), so nothing links that.
pub fn packaged_shim() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    if !is_clud_exe(&exe) {
        return Err(std::io::Error::other(format!(
            "{} is not the clud executable",
            exe.display()
        )));
    }
    Ok(exe)
}

/// The session aliases, derived from [`shim_registry::SHIMS`]: `rm`,
/// `safe-rm` (#1461) and `gh` (#1518) today. All are links to (or copies of)
/// `clud`, which dispatches on argv\[0\].
pub fn rm_alias_names() -> Vec<String> {
    shim_registry::file_names_in(ShimDir::Session)
}

fn purge_stale_aliases(dir: &Path, expected: &[String]) -> std::io::Result<()> {
    // Only known legacy aliases are ours to remove. In particular, do not
    // delete another concurrent installer's NamedTempFile before it persists.
    for alias in alias_names() {
        if expected.contains(&alias) {
            continue;
        }
        let target = dir.join(&alias);
        if std::fs::symlink_metadata(&target).is_ok() {
            if let Err(error) = crate::rm_tool::remove_link_or_file(&target) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

/// Session activation installs only the active aliases, keeping unfinished
/// Python relays off PATH. A separate directory also avoids activating
/// previously extracted Python aliases.
pub fn install_rm_at(home: &Path, source: &Path) -> std::io::Result<PathBuf> {
    use fs4::fs_std::FileExt;
    use std::fs::OpenOptions;
    use std::sync::Mutex;

    static LOCAL_INSTALL_LOCK: Mutex<()> = Mutex::new(());
    let _local_guard = LOCAL_INSTALL_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let state = home.join(".clud/state");
    std::fs::create_dir_all(&state)?;
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(state.join(".rm-shim.lock"))?;
    FileExt::lock_exclusive(&lock)?;
    if std::fs::metadata(source)?.len() == 0 {
        return Err(std::io::Error::other("packaged shim is empty"));
    }
    let dir = home.join(shim_registry::SESSION_SUBDIR);
    std::fs::create_dir_all(&dir)?;
    let expected = rm_alias_names();
    purge_stale_aliases(&dir, &expected)?;
    for name in expected {
        // A stale or replaced entry is renamed over atomically, so concurrent
        // installers never see a partial alias and never write through one.
        alias_link::install_alias(source, &dir.join(name))?;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_source(content: &[u8]) -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("clud");
        fs::write(&src, content).unwrap();
        (tmp, src)
    }

    /// Replace `path` the way an upgrade does: a new file renamed over it, so
    /// hardlinked aliases keep pointing at the old inode.
    fn replace_file(path: &Path, content: &[u8]) {
        let staged = path.with_extension("new");
        fs::write(&staged, content).unwrap();
        fs::rename(&staged, path).unwrap();
    }

    /// Replace an alias the way an attacker or a bad tool would: unlink and
    /// write a different file.
    fn swap_alias(path: &Path, content: &[u8]) {
        fs::remove_file(path).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn repairs_replaced_bytes_even_with_matching_sentinel() {
        let home = TempDir::new().unwrap();
        let (_source_dir, source) = make_source(b"trusted");
        extract_shims_at(home.path(), &source).unwrap();
        let target = home.path().join(SHIMS_SUBDIR).join(&alias_names()[0]);
        swap_alias(&target, b"replaced");
        assert_eq!(extract_shims_at(home.path(), &source).unwrap(), 1);
        assert_eq!(fs::read(target).unwrap(), b"trusted");
    }

    #[test]
    fn session_installs_only_active_aliases_and_repairs_replacement() {
        let home = TempDir::new().unwrap();
        let (_source_dir, source) = make_source(b"trusted");
        let dir = install_rm_at(home.path(), &source).unwrap();
        for name in rm_alias_names() {
            let target = dir.join(&name);
            swap_alias(&target, b"replacement");
            install_rm_at(home.path(), &source).unwrap();
            assert_eq!(fs::read(&target).unwrap(), b"trusted", "{name}");
        }
        let mut names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let mut expected: Vec<String> = rm_alias_names().iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(names, expected, "no Python relay is activated");
    }

    #[test]
    fn concurrent_session_installs_repair_gh_without_deleting_staging_files() {
        let home = TempDir::new().unwrap();
        let (_source_dir, source) = make_source(b"trusted");
        let dir = install_rm_at(home.path(), &source).unwrap();
        let gh = dir.join(shim_registry::file_name("gh"));
        swap_alias(&gh, b"replacement");
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| install_rm_at(home.path(), &source)))
                .collect();
            for worker in workers {
                worker.join().unwrap().unwrap();
            }
        });
        assert_eq!(fs::read(gh).unwrap(), b"trusted");
    }

    #[test]
    fn a_process_that_is_not_clud_links_nothing() {
        let home = TempDir::new().unwrap();
        let other = home
            .path()
            .join(shim_registry::file_name("some-test-harness"));
        fs::write(&other, b"x").unwrap();
        let mut env = vec![("PATH".to_string(), String::new())];
        assert!(!prepare_session_shims_at(home.path(), &other, &mut env).unwrap());
        assert!(!home.path().join(SHIMS_SUBDIR).exists());
    }

    #[test]
    fn an_upgraded_clud_refreshes_every_session_alias() {
        let home = TempDir::new().unwrap();
        let (_source_dir, source) = make_source(b"v1");
        let dir = install_rm_at(home.path(), &source).unwrap();
        replace_file(&source, b"v2 is different");
        install_rm_at(home.path(), &source).unwrap();
        for name in rm_alias_names() {
            assert_eq!(
                fs::read(dir.join(&name)).unwrap(),
                b"v2 is different",
                "{name}"
            );
        }
    }

    #[test]
    fn alias_names_are_platform_appropriate() {
        let names = alias_names();
        assert!(!names.is_empty());
        #[cfg(windows)]
        assert!(names.iter().any(|n| n.ends_with(".exe")));
        #[cfg(not(windows))]
        assert!(names.iter().all(|n| !n.ends_with(".exe")));
    }

    #[test]
    fn extract_creates_aliases() {
        let home = TempDir::new().unwrap();
        let (_src_dir, src) = make_source(b"#!fake shim binary\n");
        let count = extract_shims_at(home.path(), &src).unwrap();
        assert_eq!(count, alias_names().len());
        let shims = home.path().join(SHIMS_SUBDIR);
        for alias in alias_names() {
            assert!(shims.join(&alias).is_file(), "missing alias {alias}");
        }
        assert!(!shims.join(".shim-hash").exists(), "no byte-hash sentinel");
    }

    #[test]
    fn extract_is_noop_when_hash_matches() {
        let home = TempDir::new().unwrap();
        let (_src_dir, src) = make_source(b"fake shim v1");
        // First pass: writes all aliases.
        assert_eq!(
            extract_shims_at(home.path(), &src).unwrap(),
            alias_names().len()
        );
        // Second pass with identical source: nothing should change.
        assert_eq!(extract_shims_at(home.path(), &src).unwrap(), 0);
    }

    #[test]
    fn extract_refreshes_when_source_changes() {
        let home = TempDir::new().unwrap();
        let (src_dir, src) = make_source(b"v1");
        extract_shims_at(home.path(), &src).unwrap();
        // Upgrade: a new `clud` replaces the old one.
        replace_file(&src, b"v2-different");
        let count = extract_shims_at(home.path(), &src).unwrap();
        assert_eq!(
            count,
            alias_names().len(),
            "all aliases should refresh on drift"
        );
        // Confirm new bytes landed.
        let shims = home.path().join(SHIMS_SUBDIR);
        let first_alias = shims.join(&alias_names()[0]);
        assert_eq!(fs::read(&first_alias).unwrap(), b"v2-different");
        let _ = src_dir; // keep tmpdir alive
    }

    #[test]
    fn extract_no_op_when_source_missing() {
        let home = TempDir::new().unwrap();
        let nonexistent = home.path().join("not-there");
        let count = extract_shims_at(home.path(), &nonexistent).unwrap();
        assert_eq!(count, 0);
        // Dir should still be created — the launch path can populate
        // it later when the bundled binary becomes available.
        assert!(home.path().join(SHIMS_SUBDIR).is_dir());
    }

    #[test]
    fn shims_dir_resolves_under_home() {
        let resolved = shims_dir();
        // Best-effort: if home exists, the path should end with the suffix.
        if let Some(p) = resolved {
            let s = p.to_string_lossy();
            assert!(s.ends_with("shims") || s.contains("shims"), "got {s}");
        }
    }

    #[test]
    fn prepare_session_shims_extracts_aliases_and_prepends_path() {
        let home = TempDir::new().unwrap();
        let bin = home.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let current_exe = bin.join(shim_registry::file_name("clud"));
        let python = bin.join(shim_registry::file_name("python3"));
        fs::write(&current_exe, b"clud").unwrap();
        fs::write(&python, b"python").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&python, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut env = vec![("PATH".to_string(), bin.to_string_lossy().into_owned())];

        let prepared = prepare_session_shims_at(home.path(), &current_exe, &mut env).unwrap();

        assert!(prepared);
        let installed = home.path().join(SHIMS_SUBDIR);
        for alias in alias_names() {
            assert!(installed.join(&alias).is_file(), "missing {alias}");
        }
        let path = env
            .iter()
            .find(|(key, _)| key == "PATH")
            .unwrap()
            .1
            .as_str();
        assert!(
            path.starts_with(installed.to_string_lossy().as_ref()),
            "got {path}"
        );
        assert_eq!(
            env.iter()
                .find(|(key, _)| key == shim_registry::PYTHON_TARGET_KEY)
                .map(|(_, value)| value.as_str()),
            Some(python.to_string_lossy().as_ref())
        );
        assert!(
            env.iter()
                .any(|(key, value)| key == shim_registry::ABI_KEY
                    && value == shim_registry::SHIM_ABI),
            "the prepared session carries the shim ABI stamp"
        );

        assert!(prepare_session_shims_at(home.path(), &current_exe, &mut env).unwrap());
        assert_eq!(
            env.iter()
                .find(|(key, _)| key == shim_registry::PYTHON_TARGET_KEY)
                .map(|(_, value)| value.as_str()),
            Some(python.to_string_lossy().as_ref()),
            "a nested clud launch must not resolve the alias back to itself"
        );
    }
}
