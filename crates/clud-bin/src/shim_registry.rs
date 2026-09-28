//! The one list of `clud-shim` aliases, and the session contract they share
//! (#1546).
//!
//! Every alias name `clud-shim` answers to is one [`ShimSpec`] row in
//! [`SHIMS`]. The binary's single dispatch path (`bin/clud_shim.rs`, module
//! `dispatch`), the alias installers ([`crate::shim_install`]) and the child
//! env policy ([`crate::shim_session::activate_rm`]) all derive from it, so a
//! new shim is one row here plus one handler arm, which the compiler forces
//! through the exhaustive [`ShimKind`] match.
//!
//! The contract: a `Passthrough` shim runs its clud behavior only inside a
//! valid session, meaning [`ABI_KEY`] equals [`SHIM_ABI`] and the shim's own
//! target keys are present and sane. Otherwise it execs the next same-named
//! binary on PATH after the shim directories ([`next_on_path`]). The alias
//! directories are shared by every live session of every installed clud
//! version, so a session started by an older clud routinely meets a newer
//! alias; the ABI stamp is what makes that meeting fail open instead of
//! failing closed. See `docs/architecture/shim-dispatch.md`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Session stamp: the shim ABI the launching clud activated. Absent or
/// different means the env was built by another clud version, and every
/// `Passthrough` shim relays to the real binary.
pub const ABI_KEY: &str = "CLUD_SHIM_ABI";
/// Bump when a session key changes meaning. Adding a shim or a key does not
/// need a bump: an older session simply lacks the new key and passes through.
pub const SHIM_ABI: &str = "1";
/// The session alias directory; also re-prepended by the generated BASH_ENV
/// after a login shell resets PATH.
pub const SESSION_DIR_KEY: &str = "CLUD_RM_SHIM_DIR";
/// Real `python` the launching clud resolved before PATH was rewritten.
pub const PYTHON_TARGET_KEY: &str = "CLUD_PYTHON_SHIM_TARGET";
/// Real `gh` the launching clud resolved before PATH was rewritten.
pub const GH_TARGET_KEY: &str = "CLUD_GH_SHIM_TARGET";
/// `1` when the `gh` alias has a target; read by the command guard.
pub const GH_ACTIVE_KEY: &str = "CLUD_GH_SHIM_ACTIVE";
/// `0` turns the `gh pr checks --watch` upgrade off.
pub const GH_FAIL_FAST_KEY: &str = "CLUD_GH_SHIM_FAIL_FAST";
/// The clud executable the `gh` watch upgrade runs the bundled watcher with.
pub const CLUD_EXE_KEY: &str = "CLUD_EXE";

/// Every session key a shim reads. The guard test in `bin/clud_shim.rs`
/// refuses these names anywhere in the binary outside its `dispatch` module.
pub const SESSION_KEYS: &[&str] = &[
    ABI_KEY,
    SESSION_DIR_KEY,
    PYTHON_TARGET_KEY,
    GH_TARGET_KEY,
    GH_ACTIVE_KEY,
    GH_FAIL_FAST_KEY,
];

/// Interpreter aliases, prepared by `shim_install::prepare_current_session`.
pub const INTERPRETER_SUBDIR: &str = ".clud/state/shims";
/// Session aliases, installed by `shim_install::install_rm_at`.
pub const SESSION_SUBDIR: &str = ".clud/state/rm-shim";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimKind {
    Python,
    Gh,
    Rm,
    SafeRm,
}

/// What a shim does without a valid session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// Exec the next same-named binary on PATH.
    Passthrough,
    /// The shim is a clud command with its own out-of-session mode.
    Native,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimDir {
    Interpreter,
    Session,
}

impl ShimDir {
    pub fn subdir(self) -> &'static str {
        match self {
            ShimDir::Interpreter => INTERPRETER_SUBDIR,
            ShimDir::Session => SESSION_SUBDIR,
        }
    }
}

#[derive(Debug)]
pub struct ShimSpec {
    pub name: &'static str,
    pub kind: ShimKind,
    /// Keys the dispatch path reads to validate this shim's session.
    pub session_keys: &'static [&'static str],
    pub fallback: Fallback,
    /// Alias directories this name is installed into.
    pub dirs: &'static [ShimDir],
}

pub const SHIMS: &[ShimSpec] = &[
    ShimSpec {
        name: "python",
        kind: ShimKind::Python,
        session_keys: &[ABI_KEY, PYTHON_TARGET_KEY],
        fallback: Fallback::Passthrough,
        dirs: &[ShimDir::Interpreter],
    },
    ShimSpec {
        name: "python3", // python-name-lint: allow (the alias clud installs)
        kind: ShimKind::Python,
        session_keys: &[ABI_KEY, PYTHON_TARGET_KEY],
        fallback: Fallback::Passthrough,
        dirs: &[ShimDir::Interpreter],
    },
    ShimSpec {
        name: "gh",
        kind: ShimKind::Gh,
        session_keys: &[ABI_KEY, GH_TARGET_KEY, GH_FAIL_FAST_KEY],
        fallback: Fallback::Passthrough,
        dirs: &[ShimDir::Session],
    },
    ShimSpec {
        name: "rm",
        kind: ShimKind::Rm,
        session_keys: &[ABI_KEY, SESSION_DIR_KEY],
        fallback: Fallback::Passthrough,
        dirs: &[ShimDir::Interpreter, ShimDir::Session],
    },
    ShimSpec {
        name: crate::rm_tool::COMMAND,
        kind: ShimKind::SafeRm,
        session_keys: &[],
        fallback: Fallback::Native,
        dirs: &[ShimDir::Interpreter, ShimDir::Session],
    },
];

/// The row for an invoked name: argv\[0\]'s file name, with `.exe` ignored
/// (case-insensitively) so Windows and POSIX spell a shim the same way.
pub fn lookup(argv0: &OsStr) -> Option<&'static ShimSpec> {
    let name = invoked_name(argv0)?;
    SHIMS.iter().find(|spec| spec.name == name)
}

/// argv\[0\]'s file name without a trailing `.exe`, split on either
/// separator so a Windows path parses the same on every host.
pub fn invoked_name(argv0: &OsStr) -> Option<String> {
    let raw = argv0.to_str()?;
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let lower = base.to_ascii_lowercase();
    let name = if lower.ends_with(".exe") {
        &base[..base.len() - 4]
    } else {
        base
    };
    (!name.is_empty()).then(|| name.to_string())
}

/// The on-disk file name for `name` on this platform.
pub fn file_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// File names installed into `dir`, in registry order.
pub fn file_names_in(dir: ShimDir) -> Vec<String> {
    SHIMS
        .iter()
        .filter(|spec| spec.dirs.contains(&dir))
        .map(|spec| file_name(spec.name))
        .collect()
}

/// Directories whose entries are clud aliases, never real binaries: the
/// shim's own directory, the session's alias directory, and both managed
/// directories under `home`.
pub fn shim_dirs(self_exe: &Path, session_dir: Option<&Path>, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(parent) = canonical(self_exe).parent() {
        dirs.push(parent.to_path_buf());
    }
    dirs.extend(session_dir.map(canonical));
    if let Some(home) = home {
        for dir in [ShimDir::Interpreter, ShimDir::Session] {
            dirs.push(canonical(&home.join(dir.subdir())));
        }
    }
    dirs.dedup();
    dirs
}

/// Whether `target` can stand in for the real binary: absolute, an
/// executable file, outside every shim directory, and not a copy of the
/// running shim.
pub fn valid_target(target: &Path, self_exe: &Path, shim_dirs: &[PathBuf]) -> bool {
    if !target.is_absolute() || !executable(target) {
        return false;
    }
    let resolved = canonical(target);
    let in_shim_dir = resolved
        .parent()
        .is_some_and(|parent| shim_dirs.iter().any(|dir| dir == parent))
        || target
            .parent()
            .is_some_and(|parent| shim_dirs.iter().any(|dir| *dir == canonical(parent)));
    !in_shim_dir && !is_self(&resolved, self_exe)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextOnPathError {
    /// Strict mode only: the shim's directory is not on PATH.
    ShimDirMissing,
    NotFound,
}

/// The next executable named `file_name` on `path_env`, skipping every
/// directory in `shim_dirs` and any copy of `self_exe`.
///
/// The search starts after the first PATH entry that is the shim's own
/// directory, as a shell's lookup would have continued. When that entry is
/// absent (the shim was run by absolute path), `strict` reports
/// [`NextOnPathError::ShimDirMissing`] and non-strict searches all of PATH.
/// Each hop moves strictly later on PATH and never selects a shim, so a
/// chain of shims cannot loop. The returned path keeps the PATH spelling,
/// so a multi-call binary (BusyBox's `rm` symlink) sees its applet name.
pub fn next_on_path(
    file_name: &str,
    path_env: &OsStr,
    self_exe: &Path,
    shim_dirs: &[PathBuf],
    strict: bool,
) -> Result<PathBuf, NextOnPathError> {
    let own_dir = canonical(self_exe).parent().map(Path::to_path_buf);
    let entries: Vec<PathBuf> = std::env::split_paths(path_env).collect();
    let start = entries
        .iter()
        .position(|dir| own_dir.as_deref() == Some(canonical(dir).as_path()));
    let start = match (start, strict) {
        (Some(index), _) => index + 1,
        (None, true) => return Err(NextOnPathError::ShimDirMissing),
        (None, false) => 0,
    };
    scan(&entries[start..], file_name, self_exe, shim_dirs).ok_or(NextOnPathError::NotFound)
}

/// The first executable named `file_name` anywhere on `path_env`, with the
/// same exclusions as [`next_on_path`]. For resolving a real binary before
/// the alias directory is on PATH.
pub fn first_on_path(
    file_name: &str,
    path_env: &OsStr,
    self_exe: &Path,
    shim_dirs: &[PathBuf],
) -> Option<PathBuf> {
    let entries: Vec<PathBuf> = std::env::split_paths(path_env).collect();
    scan(&entries, file_name, self_exe, shim_dirs)
}

fn scan(
    entries: &[PathBuf],
    file_name: &str,
    self_exe: &Path,
    shim_dirs: &[PathBuf],
) -> Option<PathBuf> {
    for dir in entries {
        if dir.as_os_str().is_empty() || !dir.is_absolute() {
            continue;
        }
        if shim_dirs.iter().any(|shim| *shim == canonical(dir)) {
            continue;
        }
        let candidate = dir.join(file_name);
        if executable(&candidate) && !is_self(&canonical(&candidate), self_exe) {
            return Some(candidate);
        }
    }
    None
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn is_self(resolved: &Path, self_exe: &Path) -> bool {
    let me = canonical(self_exe);
    resolved == me || same_bytes(resolved, &me)
}

fn same_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(am), Ok(bm)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    am.len() == bm.len() && std::fs::read(a).ok() == std::fs::read(b).ok()
}

pub fn executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        // A Windows App Execution Alias (the Store `python.exe`) is a reparse
        // point that cannot be followed, yet CreateProcess runs it.
        return cfg!(windows) && path.symlink_metadata().is_ok_and(|meta| !meta.is_dir());
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
    use std::ffi::OsString;
    use tempfile::TempDir;

    fn write_exe(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn path_of(dirs: &[&Path]) -> OsString {
        std::env::join_paths(dirs).unwrap()
    }

    #[test]
    fn names_are_unique_and_lookup_ignores_exe_and_directories() {
        let mut names: Vec<_> = SHIMS.iter().map(|spec| spec.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), SHIMS.len());
        for spec in SHIMS {
            for argv0 in [
                spec.name.to_string(),
                format!("{}.exe", spec.name),
                format!("{}.EXE", spec.name),
                format!("/a/b/{}", spec.name),
                format!("C:\\a\\{}.exe", spec.name),
            ] {
                assert_eq!(lookup(OsStr::new(&argv0)).map(|s| s.name), Some(spec.name));
            }
        }
        assert!(lookup(OsStr::new("clud-shim")).is_none());
    }

    #[test]
    fn every_safe_alias_the_deletion_rules_install_is_registered() {
        for alias in crate::deletion_rules::generated().safe_aliases {
            let spec = SHIMS.iter().find(|spec| spec.name == alias);
            assert_eq!(spec.map(|s| s.kind), Some(ShimKind::SafeRm), "{alias}");
        }
    }

    #[test]
    fn only_safe_rm_has_a_native_mode() {
        for spec in SHIMS {
            assert_eq!(
                spec.fallback == Fallback::Native,
                spec.kind == ShimKind::SafeRm,
                "{}",
                spec.name
            );
        }
    }

    /// Session key names are spelled once, here. Every other module goes
    /// through the constants, so the guard in `bin/clud_shim.rs` can find
    /// each reader by name.
    #[test]
    fn session_key_literals_live_only_in_the_registry() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut pending = vec![src];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if !name.ends_with(".rs")
                    || name == "shim_registry.rs"
                    || name.ends_with("_tests.rs")
                {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                let production = text.split("#[cfg(test)]").next().unwrap();
                for key in SESSION_KEYS {
                    assert!(
                        !production.contains(&format!("\"{key}\"")),
                        "{} spells {key}; use crate::shim_registry",
                        path.display()
                    );
                }
            }
        }
    }

    #[test]
    fn every_session_key_a_row_reads_is_a_known_session_key() {
        for spec in SHIMS {
            for key in spec.session_keys {
                assert!(SESSION_KEYS.contains(key), "{} reads {key}", spec.name);
            }
            if spec.fallback == Fallback::Passthrough {
                assert!(spec.session_keys.contains(&ABI_KEY), "{}", spec.name);
            }
        }
    }

    #[test]
    fn next_on_path_skips_the_shim_dir_and_copies_of_the_shim() {
        let temp = TempDir::new().unwrap();
        let shim = temp.path().join("shim").join(file_name("gh"));
        let copy = temp.path().join("copy").join(file_name("gh"));
        let real = temp.path().join("real").join(file_name("gh"));
        write_exe(&shim, b"shim");
        write_exe(&copy, b"shim");
        write_exe(&real, b"real");
        let dirs = shim_dirs(&shim, None, None);
        let path = path_of(&[
            real.parent().unwrap(),
            shim.parent().unwrap(),
            copy.parent().unwrap(),
            real.parent().unwrap(),
        ]);
        assert_eq!(
            next_on_path(&file_name("gh"), &path, &shim, &dirs, false),
            Ok(real.clone()),
            "the entry after the shim, not the one before it"
        );
        let only_shims = path_of(&[shim.parent().unwrap(), copy.parent().unwrap()]);
        assert_eq!(
            next_on_path(&file_name("gh"), &only_shims, &shim, &dirs, false),
            Err(NextOnPathError::NotFound)
        );
    }

    #[test]
    fn strict_mode_requires_the_shim_dir_on_path() {
        let temp = TempDir::new().unwrap();
        let shim = temp.path().join("shim").join(file_name("rm"));
        let real = temp.path().join("real").join(file_name("rm"));
        write_exe(&shim, b"shim");
        write_exe(&real, b"real");
        let path = path_of(&[real.parent().unwrap()]);
        assert_eq!(
            next_on_path(&file_name("rm"), &path, &shim, &[], true),
            Err(NextOnPathError::ShimDirMissing)
        );
        assert_eq!(
            next_on_path(&file_name("rm"), &path, &shim, &[], false),
            Ok(real)
        );
    }

    #[test]
    fn a_target_inside_a_shim_dir_or_equal_to_the_shim_is_invalid() {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let shim = home.join(SESSION_SUBDIR).join(file_name("gh"));
        let python_alias = home.join(INTERPRETER_SUBDIR).join(file_name("gh"));
        let copy = temp.path().join("copy").join(file_name("gh"));
        let real = temp.path().join("real").join(file_name("gh"));
        write_exe(&shim, b"shim");
        write_exe(&python_alias, b"other shim version");
        write_exe(&copy, b"shim");
        write_exe(&real, b"real");
        let dirs = shim_dirs(&shim, None, Some(&home));
        assert!(valid_target(&real, &shim, &dirs));
        for bad in [&shim, &python_alias, &copy] {
            assert!(!valid_target(bad, &shim, &dirs), "{}", bad.display());
        }
        assert!(!valid_target(Path::new("relative/gh"), &shim, &dirs));
        assert!(!valid_target(&temp.path().join("missing"), &shim, &dirs));
    }
}
