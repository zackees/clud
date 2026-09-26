//! Per-user shim extraction. Slice 4 of #406 / #412.
//!
//! Materializes the `clud-shim` binary into `~/.clud/state/shims/` under
//! the alias names downstream tooling will invoke (`python`,
//! `python3`, `python.exe`, `python3.exe`). The shim dir is **per-user**,
//! shared across all sessions and concurrent clud processes for that
//! user — extracted once, hash-gated for drift detection at upgrade.
//!
//! The installed wheel places `clud-shim` beside `clud`; startup copies it to
//! the alias directory, resolves the real Python before rewriting PATH, and
//! exports that target for the shim to execute without recursive lookup.
//!
//! Mirrors the bundled-skill installer pattern in `skills.rs`:
//! managed copies carry the `# managed-by: clud` marker so user-edited
//! files are preserved across upgrades.

use std::path::{Path, PathBuf};

pub const SHIM_TARGET_ENV_VAR: &str = "CLUD_PYTHON_SHIM_TARGET";

/// Subdirectory under the user's `~/.clud/state/` where shim aliases
/// live. Joined to the resolved state root by [`shims_dir`].
pub const SHIMS_SUBDIR: &str = ".clud/state/shims";

/// Alias filenames to install under [`SHIMS_SUBDIR`]. Each alias is a
/// copy of the same `clud-shim` binary; the shim's `current_exe_basename`
/// logic uses argv\[0\] to decide which interpreter family the caller
/// wanted.
pub fn alias_names() -> Vec<&'static str> {
    #[cfg(windows)]
    {
        vec![
            "python.exe",
            "python3.exe",
            "rm.exe",
            "rm-file.exe",
            "rm-dir.exe",
        ]
    }
    #[cfg(not(windows))]
    {
        vec!["python", "python3", "rm", "rm-file", "rm-dir"]
    }
}

/// `~/.clud/state/shims/`. Returns `None` when the user's home dir
/// cannot be resolved — callers degrade silently (the launch path
/// just skips shim install).
pub fn shims_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join(SHIMS_SUBDIR))
}

/// Testable variant — install all aliases under `home_root` from the
/// supplied `shim_source` path. Returns the count of aliases installed
/// or refreshed.
///
/// If `shim_source` does not exist or cannot be read, returns 0 — the
/// session can still proceed; the agent's `python` invocation just
/// won't route through the shim. This is the deliberate
/// graceful-fallback behavior the no-daemon case relies on.
pub fn extract_shims_at(home_root: &Path, shim_source: &Path) -> std::io::Result<usize> {
    let shims_dir = home_root.join(SHIMS_SUBDIR);
    std::fs::create_dir_all(&shims_dir)?;
    if !shim_source.is_file() {
        return Ok(0);
    }
    let source_bytes = std::fs::read(shim_source)?;
    let source_hash = blake3_short(&source_bytes);
    let mut installed = 0;
    for alias in alias_names() {
        let target = shims_dir.join(alias);
        if std::fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink())
            || needs_refresh(&target, &source_hash)
            || std::fs::read(&target).ok().as_deref() != Some(source_bytes.as_slice())
        {
            write_alias(&shims_dir, &target, &source_bytes)?;
            installed += 1;
        }
    }
    write_hash_sentinel(&shims_dir, &source_hash)?;
    Ok(installed)
}

fn native_binary_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Install the interpreter aliases and make them authoritative for one child
/// environment. The real interpreter is resolved before PATH is rewritten so
/// the shim can never select itself recursively.
pub fn prepare_session_shims_at(
    home_root: &Path,
    current_exe: &Path,
    env: &mut Vec<(String, String)>,
) -> std::io::Result<bool> {
    let Some(bin_dir) = current_exe.parent() else {
        return Ok(false);
    };
    let source = bin_dir.join(native_binary_name("clud-shim"));
    if !source.is_file() {
        return Ok(false);
    }
    let inherited_target = env
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(SHIM_TARGET_ENV_VAR))
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
            let Some(path) = crate::shim_resolve::which_python_default("python", path) else {
                return Ok(false);
            };
            path
        }
    };

    extract_shims_at(home_root, &source)?;
    let installed = home_root.join(SHIMS_SUBDIR);
    crate::shim_session::prepend_to_path(env, &installed);
    crate::shim_session::set_env(env, SHIM_TARGET_ENV_VAR, &target.to_string_lossy());
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
    for key in ["PATH", SHIM_TARGET_ENV_VAR] {
        if let Some((_, value)) = env.iter().find(|(candidate, _)| candidate == key) {
            // SAFETY: startup-only write to PATH/SHIM_TARGET, before the backend
            // is spawned and before any thread that reads the environment runs.
            unsafe { std::env::set_var(key, value) };
        }
    }
    Ok(true)
}

/// Reuse the daemon's existing BLAKE3 wrapper from `sha2` is overkill;
/// for drift detection we just hash with the standard library's
/// SipHash via a stable byte digest. Surfaced as its own function so
/// tests can verify the sentinel format without depending on a hash
/// crate that might churn.
fn blake3_short(bytes: &[u8]) -> String {
    // Hand-rolled FNV-1a — deterministic, no deps, sufficient for
    // drift detection (we only compare exact equality). Skip
    // cryptographic strength; an attacker who controls the bundled
    // binary controls everything already.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

fn write_hash_sentinel(shims_dir: &Path, hash: &str) -> std::io::Result<()> {
    let sentinel = shims_dir.join(".shim-hash");
    std::fs::write(sentinel, hash)
}

fn read_hash_sentinel(shims_dir: &Path) -> Option<String> {
    let sentinel = shims_dir.join(".shim-hash");
    std::fs::read_to_string(sentinel).ok()
}

fn needs_refresh(target: &Path, source_hash: &str) -> bool {
    if !target.is_file() {
        return true;
    }
    let Some(parent) = target.parent() else {
        return true;
    };
    let Some(installed) = read_hash_sentinel(parent) else {
        return true;
    };
    installed != source_hash
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

fn write_alias(dir: &Path, target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.write_all(bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o755))?;
    }
    temp.persist(target).map_err(|e| e.error)?;
    Ok(())
}

/// Trusted packaged sibling, never resolved through PATH or an env override.
pub fn packaged_shim() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| std::io::Error::other("executable has no parent"))?;
    Ok(dir.join(if cfg!(windows) {
        "clud-shim.exe"
    } else {
        "clud-shim"
    }))
}

/// The deletion aliases session activation installs: the child `rm` shim
/// and the agent-facing `rm-file` / `rm-dir` (#1340). All are byte copies of
/// `clud-shim`, which dispatches on argv\[0\].
pub fn rm_alias_names() -> [&'static str; 3] {
    if cfg!(windows) {
        ["rm.exe", "rm-file.exe", "rm-dir.exe"]
    } else {
        ["rm", "rm-file", "rm-dir"]
    }
}

/// Session activation installs only the deletion aliases, keeping unfinished
/// Python relays off PATH. A separate directory also avoids activating
/// previously extracted Python aliases.
pub fn install_rm_at(home: &Path, source: &Path) -> std::io::Result<PathBuf> {
    let bytes = std::fs::read(source)?;
    if bytes.is_empty() {
        return Err(std::io::Error::other("packaged shim is empty"));
    }
    let dir = home.join(".clud/state/rm-shim");
    std::fs::create_dir_all(&dir)?;
    for name in rm_alias_names() {
        let target = dir.join(name);
        // Replace the directory entry, never write through a replaced symlink.
        // NamedTempFile persists atomically and supports concurrent installers.
        if std::fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink())
            || std::fs::read(&target).ok().as_deref() != Some(bytes.as_slice())
        {
            write_alias(&dir, &target, &bytes)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
        }
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
        let src = tmp.path().join("clud-shim");
        fs::write(&src, content).unwrap();
        (tmp, src)
    }

    #[test]
    fn repairs_replaced_bytes_even_with_matching_sentinel() {
        let home = TempDir::new().unwrap();
        let (_source_dir, source) = make_source(b"trusted");
        extract_shims_at(home.path(), &source).unwrap();
        let target = home.path().join(SHIMS_SUBDIR).join(alias_names()[0]);
        fs::write(&target, b"replaced").unwrap();
        assert_eq!(extract_shims_at(home.path(), &source).unwrap(), 1);
        assert_eq!(fs::read(target).unwrap(), b"trusted");
    }

    #[test]
    fn session_installs_only_the_deletion_aliases_and_repairs_replacement() {
        let home = TempDir::new().unwrap();
        let (_source_dir, source) = make_source(b"trusted");
        let dir = install_rm_at(home.path(), &source).unwrap();
        for name in rm_alias_names() {
            let target = dir.join(name);
            fs::write(&target, b"replacement").unwrap();
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
    fn alias_names_are_platform_appropriate() {
        let names = alias_names();
        assert!(!names.is_empty());
        #[cfg(windows)]
        assert!(names.iter().any(|n| n.ends_with(".exe")));
        #[cfg(not(windows))]
        assert!(names.iter().all(|n| !n.ends_with(".exe")));
    }

    #[test]
    fn extract_creates_aliases_and_sentinel() {
        let home = TempDir::new().unwrap();
        let (_src_dir, src) = make_source(b"#!fake shim binary\n");
        let count = extract_shims_at(home.path(), &src).unwrap();
        assert_eq!(count, alias_names().len());
        let shims = home.path().join(SHIMS_SUBDIR);
        for alias in alias_names() {
            assert!(shims.join(alias).is_file(), "missing alias {alias}");
        }
        assert!(shims.join(".shim-hash").is_file());
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
        // Rewrite the source with new content.
        fs::write(&src, b"v2-different").unwrap();
        let count = extract_shims_at(home.path(), &src).unwrap();
        assert_eq!(
            count,
            alias_names().len(),
            "all aliases should refresh on drift"
        );
        // Confirm new bytes landed.
        let shims = home.path().join(SHIMS_SUBDIR);
        let first_alias = shims.join(alias_names()[0]);
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
    fn blake3_short_is_stable_across_calls() {
        let a = blake3_short(b"hello");
        let b = blake3_short(b"hello");
        assert_eq!(a, b);
        assert_ne!(blake3_short(b"hello"), blake3_short(b"goodbye"));
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
        let current_exe = bin.join(native_binary_name("clud"));
        let shim = bin.join(native_binary_name("clud-shim"));
        let python = bin.join(native_binary_name("python3"));
        fs::write(&current_exe, b"clud").unwrap();
        fs::write(&shim, b"shim").unwrap();
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
            assert!(installed.join(alias).is_file(), "missing {alias}");
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
                .find(|(key, _)| key == SHIM_TARGET_ENV_VAR)
                .map(|(_, value)| value.as_str()),
            Some(python.to_string_lossy().as_ref())
        );

        assert!(prepare_session_shims_at(home.path(), &current_exe, &mut env).unwrap());
        assert_eq!(
            env.iter()
                .find(|(key, _)| key == SHIM_TARGET_ENV_VAR)
                .map(|(_, value)| value.as_str()),
            Some(python.to_string_lossy().as_ref()),
            "a nested clud launch must not resolve the alias back to itself"
        );
    }
}
