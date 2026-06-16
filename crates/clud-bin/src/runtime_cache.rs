//! Canonical per-version runtime cache for `clud.exe` (issue #333).
//!
//! Feature-flagged cache hop for #333. This module owns the cache-path
//! computation, the cross-platform "am I running from the cache?"
//! predicate, the double-checked file-locked [`prepare_cached_clud_in`]
//! copy-once helper, and the opt-in re-exec hop. The hop is gated
//! behind `CLUD_USE_RUNTIME_CACHE=1` so production behavior stays
//! unchanged until the default-on phase.
//!
//! Design summary (full version in issue #333):
//! - Layout: `~/.clud/runtime/clud-<version>/<binary-name>`.
//! - On first invocation per version, copy `current_exe()` into the
//!   cache dir under a file lock; subsequent invocations re-exec
//!   from the cache hit and skip the trampoline entirely.
//! - Direct port of zccache's `runtime-binaries/` pattern
//!   (`runtime_binaries_dir` / `prepare_daemon_exe` /
//!   `exe_is_under_runtime_binaries` in zccache's
//!   `crates/zccache/src/{cli/runtime.rs, daemon/trampoline.rs}`),
//!   with the cache key changed from per-launch random to per-version
//!   so subsequent invocations are zero-I/O cache hits.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use fs4::fs_std::FileExt;

/// Subdirectory under `~/.clud/` where per-version cached binaries
/// live. Mirrors zccache's `runtime-binaries/` convention.
const RUNTIME_SUBDIR: &str = "runtime";

/// Opt-in gate for the runtime-cache hop. Default off until the
/// re-exec path has soaked in real PTY / backend workflows.
const CLUD_USE_RUNTIME_CACHE: &str = "CLUD_USE_RUNTIME_CACHE";

/// Existing escape hatch. During the opt-in phase it disables both the legacy
/// unlock trampoline and the new runtime-cache hop.
const CLUD_NO_UNLOCK: &str = "CLUD_NO_UNLOCK";

/// Compile-time version stamp consumed by [`runtime_cache_dir`].
const CLUD_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Returns `<home>/.clud/runtime/clud-<version>/` — the cache dir
/// for this specific clud version. Per-version namespacing so that
/// `pip install --upgrade clud` lands on a new cache dir, leaving
/// the old one orphaned (to be GC'd lazily in Phase 3).
pub fn runtime_cache_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(
        home.join(".clud")
            .join(RUNTIME_SUBDIR)
            .join(format!("clud-{CLUD_VERSION}")),
    )
}

/// Filename for the cached binary inside [`runtime_cache_dir`].
/// Includes the `.exe` extension on Windows.
pub fn cached_clud_binary_name() -> &'static str {
    if cfg!(windows) {
        "clud.exe"
    } else {
        "clud"
    }
}

/// Full path to the cached binary:
/// `<runtime_cache_dir>/<cached_clud_binary_name>`.
pub fn cached_clud_path() -> Option<PathBuf> {
    Some(runtime_cache_dir()?.join(cached_clud_binary_name()))
}

/// Returns true when the runtime-cache hop should run.
pub fn runtime_cache_hop_enabled() -> bool {
    runtime_cache_hop_enabled_from_vars(
        std::env::var_os(CLUD_USE_RUNTIME_CACHE).is_some(),
        std::env::var_os(CLUD_NO_UNLOCK).is_some(),
        cfg!(debug_assertions),
    )
}

fn runtime_cache_hop_enabled_from_vars(
    use_runtime_cache: bool,
    no_unlock: bool,
    debug_assertions: bool,
) -> bool {
    use_runtime_cache && !no_unlock && !debug_assertions
}

/// If `CLUD_USE_RUNTIME_CACHE=1` is set, ensure this clud binary is
/// cached under `~/.clud/runtime/clud-<version>/` and re-exec from
/// there before normal startup work begins.
///
/// Returns normally only when the hop is disabled, the current process
/// is already running from the runtime cache, or preparing/spawning the
/// cached binary fails. On a successful hop this function replaces the
/// process on Unix, or waits for the child and exits with its status on
/// Windows.
pub fn hop_to_runtime_cache_if_enabled() -> io::Result<()> {
    if !runtime_cache_hop_enabled() {
        return Ok(());
    }

    let current_exe = std::env::current_exe()?;
    if exe_is_under_clud_runtime(&current_exe) {
        return Ok(());
    }

    let cached = prepare_cached_clud(&current_exe)?;
    if paths_equivalent(&current_exe, &cached) {
        return Ok(());
    }

    reexec_from_cached_binary(&cached)
}

fn paths_equivalent(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn reexec_from_cached_binary(cached: &Path) -> io::Result<()> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = Command::new(cached).args(&args).exec();
        Err(err)
    }

    #[cfg(not(unix))]
    {
        let status = Command::new(cached).args(&args).status()?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

/// True if `exe` resolves into a path under `~/.clud/runtime/`.
/// Used by the trampoline's `unlock_exe()` short-circuit (Phase 2):
/// when clud is running from the canonical cache, the in-place
/// rename is a no-op because the cache path is never the install
/// path that `pip install --upgrade clud` overwrites.
///
/// Canonicalizes both sides to be robust against symlinks and
/// Windows 8.3 short-name tilde expansion. Returns `false` if home
/// dir resolution fails or either path is not canonicalizable.
pub fn exe_is_under_clud_runtime(exe: &Path) -> bool {
    let Some(runtime_root) = dirs::home_dir().map(|h| h.join(".clud").join(RUNTIME_SUBDIR)) else {
        return false;
    };
    exe_is_under_runtime_root(exe, &runtime_root)
}

/// Test seam for [`exe_is_under_clud_runtime`]: same predicate but
/// the runtime root is supplied explicitly so unit tests can point
/// at a `tempfile::TempDir` instead of the user's real `~/.clud/`.
pub fn exe_is_under_runtime_root(exe: &Path, runtime_root: &Path) -> bool {
    let Ok(runtime_canon) = fs::canonicalize(runtime_root) else {
        return false;
    };
    let Ok(exe_canon) = fs::canonicalize(exe) else {
        return false;
    };
    exe_canon.starts_with(&runtime_canon)
}

/// Ensure the cached binary exists at [`cached_clud_path`]. If it
/// already exists, returns its path (fast path, one `stat`). If not,
/// acquires an exclusive file lock at `<dir>/.lock`, re-checks under
/// the lock (the "double-check"), and on a real cache miss copies
/// `source` into a temp sibling then atomically renames into place.
///
/// The fast-path `exists()` check without the lock is correct
/// because the slow path renames atomically — observers can only
/// see "doesn't exist" or "fully written," never a partial file.
///
/// Returns the cached path on success. Pure I/O — does not re-exec
/// or otherwise modify the current process. The re-exec hop is
/// Phase 2 and lives in a separate PR.
pub fn prepare_cached_clud(source: &Path) -> io::Result<PathBuf> {
    let cached = cached_clud_path().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no home dir for clud runtime cache",
        )
    })?;
    let dir = cached
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "cached path has no parent"))?;
    prepare_cached_clud_in(source, dir, cached_clud_binary_name())
}

/// Test seam for [`prepare_cached_clud`]: same double-checked
/// locking + copy logic but the cache `dir` and `binary_name` are
/// supplied so unit tests can point at a `tempfile::TempDir`.
pub fn prepare_cached_clud_in(source: &Path, dir: &Path, binary_name: &str) -> io::Result<PathBuf> {
    let cached = dir.join(binary_name);

    if cached.exists() {
        return Ok(cached);
    }

    fs::create_dir_all(dir)?;

    let lock_path = dir.join(".lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    lock_file.lock_exclusive()?;

    // Double-check: another process may have completed the copy
    // while we were waiting for the lock.
    if cached.exists() {
        return Ok(cached);
    }

    // Copy `source` to a temp sibling, then atomic rename into
    // place. The temp name carries our PID so concurrent first
    // copies (rare — the lock serializes inter-process, but
    // belt-and-suspenders for cross-process races on systems where
    // the advisory lock is best-effort) don't collide.
    let temp_name = format!("{binary_name}.tmp.{}", std::process::id());
    let temp_path = dir.join(&temp_name);
    fs::copy(source, &temp_path)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&temp_path)?.permissions();
        // Preserve executable bit; `fs::copy` already copies mode on
        // Unix but be defensive in case `source` came from a tarball
        // extract that stripped it.
        perms.set_mode(perms.mode() | 0o100);
        fs::set_permissions(&temp_path, perms)?;
    }

    fs::rename(&temp_path, &cached)?;

    Ok(cached)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn runtime_cache_dir_format_contains_version_and_clud_prefix() {
        let dir = runtime_cache_dir().expect("home dir resolvable on test host");
        let dir_str = dir.to_string_lossy();
        assert!(dir_str.contains(".clud"));
        assert!(dir_str.contains(RUNTIME_SUBDIR));
        assert!(dir_str.contains(&format!("clud-{CLUD_VERSION}")));
    }

    #[test]
    fn cached_binary_name_matches_platform_extension() {
        let name = cached_clud_binary_name();
        if cfg!(windows) {
            assert_eq!(name, "clud.exe");
        } else {
            assert_eq!(name, "clud");
        }
    }

    #[test]
    fn cached_clud_path_combines_dir_and_binary_name() {
        let path = cached_clud_path().expect("home dir resolvable");
        let dir = runtime_cache_dir().expect("home dir resolvable");
        assert_eq!(path, dir.join(cached_clud_binary_name()));
    }

    #[test]
    fn runtime_cache_hop_enabled_requires_opt_in() {
        assert!(!runtime_cache_hop_enabled_from_vars(false, false, false));
    }

    #[test]
    fn runtime_cache_hop_enabled_respects_existing_unlock_escape_hatch() {
        assert!(!runtime_cache_hop_enabled_from_vars(true, true, false));
    }

    #[test]
    fn runtime_cache_hop_enabled_stays_off_for_debug_builds() {
        assert!(!runtime_cache_hop_enabled_from_vars(true, false, true));
    }

    #[test]
    fn runtime_cache_hop_enabled_when_opted_in_without_escape_hatch() {
        assert!(runtime_cache_hop_enabled_from_vars(true, false, false));
    }

    #[test]
    fn exe_under_runtime_root_true_when_exe_lives_inside_root() {
        let tmp = TempDir::new().expect("tempdir");
        let runtime_root = tmp.path().join("runtime");
        let version_dir = runtime_root.join("clud-test");
        fs::create_dir_all(&version_dir).expect("mkdir");
        let exe = version_dir.join(cached_clud_binary_name());
        fs::write(&exe, b"fake").expect("write");

        assert!(exe_is_under_runtime_root(&exe, &runtime_root));
    }

    #[test]
    fn exe_under_runtime_root_false_when_exe_lives_elsewhere() {
        let tmp = TempDir::new().expect("tempdir");
        let runtime_root = tmp.path().join("runtime");
        fs::create_dir_all(&runtime_root).expect("mkdir runtime");
        let unrelated = tmp.path().join("other");
        fs::create_dir_all(&unrelated).expect("mkdir other");
        let exe = unrelated.join(cached_clud_binary_name());
        fs::write(&exe, b"fake").expect("write");

        assert!(!exe_is_under_runtime_root(&exe, &runtime_root));
    }

    #[test]
    fn exe_under_runtime_root_false_when_runtime_root_missing() {
        let tmp = TempDir::new().expect("tempdir");
        let runtime_root = tmp.path().join("does-not-exist");
        let exe = tmp.path().join("clud");
        fs::write(&exe, b"fake").expect("write");

        // Canonicalization fails on the missing runtime root, so the
        // predicate must return false rather than panic.
        assert!(!exe_is_under_runtime_root(&exe, &runtime_root));
    }

    #[test]
    fn prepare_cached_clud_in_first_call_copies_source_into_cache() {
        let tmp = TempDir::new().expect("tempdir");
        let source = tmp.path().join("source-clud");
        fs::write(&source, b"binary-content-v1").expect("write source");
        let cache_dir = tmp.path().join("cache");

        let cached = prepare_cached_clud_in(&source, &cache_dir, "clud").expect("first prepare");

        assert_eq!(cached, cache_dir.join("clud"));
        assert!(cached.exists(), "cache hit must exist after first prepare");
        assert_eq!(
            fs::read(&cached).expect("read cached"),
            b"binary-content-v1"
        );
    }

    #[test]
    fn prepare_cached_clud_in_second_call_is_zero_copy_cache_hit() {
        let tmp = TempDir::new().expect("tempdir");
        let source = tmp.path().join("source-clud");
        fs::write(&source, b"binary-content-v1").expect("write source");
        let cache_dir = tmp.path().join("cache");

        let first = prepare_cached_clud_in(&source, &cache_dir, "clud").expect("first prepare");
        // Mutate the source after the first prepare. If the second
        // call hits the slow path it would copy the new content;
        // the fast path must not — the cached file is canonical for
        // this version.
        fs::write(&source, b"mutated-after-cache").expect("mutate source");

        let second = prepare_cached_clud_in(&source, &cache_dir, "clud").expect("second prepare");

        assert_eq!(first, second);
        assert_eq!(
            fs::read(&second).expect("read cached"),
            b"binary-content-v1",
            "second prepare must hit cache, not re-copy mutated source"
        );
    }

    #[test]
    fn prepare_cached_clud_in_creates_missing_parent_dir() {
        let tmp = TempDir::new().expect("tempdir");
        let source = tmp.path().join("source-clud");
        fs::write(&source, b"x").expect("write source");
        // cache_dir explicitly does not exist yet.
        let cache_dir = tmp.path().join("deep").join("nested").join("cache");

        let cached = prepare_cached_clud_in(&source, &cache_dir, "clud").expect("prepare");

        assert!(cache_dir.is_dir(), "cache dir must be created");
        assert!(cached.exists());
    }

    #[test]
    fn prepare_cached_clud_in_leaves_no_temp_file_on_success() {
        let tmp = TempDir::new().expect("tempdir");
        let source = tmp.path().join("source-clud");
        fs::write(&source, b"x").expect("write source");
        let cache_dir = tmp.path().join("cache");

        prepare_cached_clud_in(&source, &cache_dir, "clud").expect("prepare");

        // The atomic-rename pattern must clean up the temp sibling.
        let pid = std::process::id();
        let temp_path = cache_dir.join(format!("clud.tmp.{pid}"));
        assert!(
            !temp_path.exists(),
            "temp sibling {} must be renamed away, not left behind",
            temp_path.display()
        );
    }
}
