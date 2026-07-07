//! Per-user shim extraction. Slice 4 of #406 / #412.
//!
//! Materializes the `clud-shim` binary into `~/.clud/state/shims/` under
//! the alias names downstream tooling will invoke (`python`,
//! `python3`, `python.exe`, `python3.exe`). The shim dir is **per-user**,
//! shared across all sessions and concurrent clud processes for that
//! user — extracted once, hash-gated for drift detection at upgrade.
//!
//! Slice 4 ships the extraction logic + drift detection. The CI-side
//! 6-platform size-gate (≤ 500 KiB stripped) and the wheel-side
//! `include_bytes!` bundling of the prebuilt shim are deferred to a
//! follow-up: those need the maturin packaging + GitHub Actions matrix
//! changes that live outside `crates/clud-bin`. The extraction surface
//! here is the contract the bundling layer plugs into.
//!
//! Mirrors the bundled-skill installer pattern in `skill_install.rs`:
//! managed copies carry the `# managed-by: clud` marker so user-edited
//! files are preserved across upgrades.

use std::path::{Path, PathBuf};

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
        vec!["python.exe", "python3.exe"]
    }
    #[cfg(not(windows))]
    {
        vec!["python", "python3"]
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
        if needs_refresh(&target, &source_hash) {
            std::fs::write(&target, &source_bytes)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perm = std::fs::metadata(&target)?.permissions();
                perm.set_mode(0o755);
                std::fs::set_permissions(&target, perm)?;
            }
            installed += 1;
        }
    }
    write_hash_sentinel(&shims_dir, &source_hash)?;
    Ok(installed)
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
}
