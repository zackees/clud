//! Windows trampoline: copies the binary to a cache dir so pip can always
//! overwrite the original. On Linux/macOS this is a no-op (Unix allows
//! deleting running binaries).
//!
//! Flow:
//! 1. pip installs `clud.exe` to Scripts/ — this is the trampoline
//! 2. On launch, trampoline hashes itself, copies to `Scripts/.clud-bin/<hash>.exe`
//! 3. Spawns the cached copy with all args, waits, exits with its code
//! 4. pip install can freely overwrite Scripts/clud.exe (not locked)
//! 5. Next launch: new hash → new cached copy, old ones cleaned up

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::Hasher;
use std::path::{Path, PathBuf};

/// Env var set on the cached copy so it knows it's already trampolined.
const TRAMPOLINE_ENV: &str = "_CLUD_TRAMPOLINED";

/// Try to trampoline. Returns `Some(exit_code)` if we spawned the cached
/// copy (caller should exit). Returns `None` if we ARE the cached copy
/// or trampolining is not needed (Unix).
pub fn maybe_trampoline() -> Option<i32> {
    // Only needed on Windows
    if !cfg!(target_os = "windows") {
        return None;
    }

    // If we're already the trampolined copy, run normally
    if std::env::var(TRAMPOLINE_ENV).is_ok() {
        return None;
    }

    let my_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return None,
    };

    let my_bytes = match fs::read(&my_exe) {
        Ok(b) => b,
        Err(_) => return None,
    };

    let hash = hash_bytes(&my_bytes);
    let cache_dir = cache_dir_for(&my_exe);

    if fs::create_dir_all(&cache_dir).is_err() {
        return None;
    }

    let ext = if cfg!(target_os = "windows") {
        ".exe"
    } else {
        ""
    };
    let cached_exe = cache_dir.join(format!("{hash}{ext}"));

    // Copy if our hash isn't cached yet
    if !cached_exe.is_file() {
        if let Err(e) = fs::copy(&my_exe, &cached_exe) {
            eprintln!("[clud] trampoline: failed to cache binary: {e}");
            return None; // Fall through to run directly
        }
    }

    // Clean up old cached copies (best-effort, ignore locked files)
    cleanup_old(&cache_dir, &cached_exe);

    // Spawn the cached copy with all our args
    let args: Vec<String> = std::env::args().skip(1).collect();
    let status = std::process::Command::new(&cached_exe)
        .args(&args)
        .env(TRAMPOLINE_ENV, "1")
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status();

    match status {
        Ok(s) => Some(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("[clud] trampoline: failed to exec cached binary: {e}");
            None // Fall through to run directly
        }
    }
}

/// Cache directory: `.clud-bin/` next to the executable.
fn cache_dir_for(exe: &Path) -> PathBuf {
    exe.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".clud-bin")
}

/// Fast hash of file contents.
fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    hasher.write(bytes);
    format!("{:016x}", hasher.finish())
}

/// Remove cached copies that aren't the current one.
/// Silently skips locked files (still running).
fn cleanup_old(cache_dir: &Path, keep: &Path) -> u32 {
    let mut cleaned = 0u32;
    let entries = match fs::read_dir(cache_dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep {
            continue;
        }
        let is_binary = path.extension().is_some_and(|e| e == "exe") || path.extension().is_none();
        if is_binary && fs::remove_file(&path).is_ok() {
            cleaned += 1;
        }
        // Locked files silently skipped — cleaned up next launch
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_hash_deterministic() {
        let h1 = hash_bytes(b"hello world");
        let h2 = hash_bytes(b"hello world");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_hash_different_content() {
        let h1 = hash_bytes(b"hello");
        let h2 = hash_bytes(b"world");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_cleanup_old() {
        let tmp = std::env::temp_dir().join("clud-trampoline-test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let keep = tmp.join("keep.exe");
        let old1 = tmp.join("old1.exe");
        let old2 = tmp.join("old2.exe");
        fs::write(&keep, b"keep").unwrap();
        fs::write(&old1, b"old1").unwrap();
        fs::write(&old2, b"old2").unwrap();

        let cleaned = cleanup_old(&tmp, &keep);
        assert_eq!(cleaned, 2);
        assert!(keep.is_file());
        assert!(!old1.exists());
        assert!(!old2.exists());

        let _ = fs::remove_dir_all(&tmp);
    }
}
