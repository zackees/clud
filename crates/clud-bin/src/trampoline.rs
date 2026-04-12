//! Windows trampoline: ensures pip can always overwrite clud.exe.
//!
//! Problem: On Windows, running executables are file-locked. `pip install .`
//! fails if clud is running because it can't overwrite Scripts/clud.exe.
//!
//! Solution: On launch, clud renames itself (Scripts/clud.exe → clud.exe.old.<rand>),
//! copies a fresh unlocked copy back, then runs the real work from a cached copy
//! in %LOCALAPPDATA%/clud/bin/<hash>.exe.
//!
//! Result: Scripts/clud.exe is NEVER locked. pip install always works.
//!
//! IMPORTANT: Every operation in this module is best-effort. If anything fails
//! (permissions, locked files, disk full, missing dirs), the app continues
//! normally — it just won't get the lock-free install benefit.
//!
//! On Linux/macOS: no-op (Unix allows deleting running binaries).

use std::fs;
use std::path::{Path, PathBuf};

/// Env var set on the cached copy so it knows it's already trampolined.
const TRAMPOLINE_ENV: &str = "_CLUD_TRAMPOLINED";

/// Try to trampoline. Returns `Some(exit_code)` if we spawned the cached
/// copy (caller should exit). Returns `None` if we ARE the cached copy,
/// trampolining is not needed (Unix), or any step fails (app runs directly).
pub fn maybe_trampoline() -> Option<i32> {
    if !cfg!(target_os = "windows") {
        return None;
    }

    // Already the cached copy — run normally.
    if std::env::var(TRAMPOLINE_ENV).is_ok() {
        return None;
    }

    // All of this is best-effort. If any step fails, return None
    // and the app runs directly from Scripts/clud.exe (old behavior).
    trampoline_inner()
}

/// Inner implementation — separated so we can return None on any failure
/// without duplicating error handling.
fn trampoline_inner() -> Option<i32> {
    let my_exe = std::env::current_exe().ok()?;

    // Step 1: Rename ourselves so Scripts/clud.exe becomes unlocked.
    // If this fails, pip install won't benefit but the app still works.
    unlock_self(&my_exe);

    // Step 2: Copy to global cache and spawn from there.
    let my_bytes = match fs::read(&my_exe) {
        Ok(b) => b,
        Err(_) => return None, // Can't read ourselves — just run directly.
    };
    let hash = hash_bytes(&my_bytes);
    let cache_dir = cache_dir();
    if fs::create_dir_all(&cache_dir).is_err() {
        return None; // Can't create cache dir — run directly.
    }

    let cached_exe = cache_dir.join(format!("{hash}.exe"));
    if !cached_exe.is_file() && fs::write(&cached_exe, &my_bytes).is_err() {
        return None; // Can't write cache — run directly.
    }

    // Step 3: Spawn the cached copy with all args.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut child = match std::process::Command::new(&cached_exe)
        .args(&args)
        .env(TRAMPOLINE_ENV, "1")
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return None, // Can't spawn cached copy — run directly.
    };

    // Step 4: GC in background now that the child is running.
    // Fire and forget — if the thread panics or GC fails, we don't care.
    let gc_exe = my_exe.clone();
    let gc_cache_dir = cache_dir.clone();
    let gc_keep = cached_exe.clone();
    std::thread::spawn(move || {
        gc_stale_files(&gc_exe);
        cleanup_old_cached(&gc_cache_dir, &gc_keep);
    });

    // Step 5: Wait for child to finish.
    match child.wait() {
        Ok(s) => Some(s.code().unwrap_or(1)),
        Err(_) => Some(1), // Child wait failed — treat as error exit.
    }
}

/// GC stale .old files next to the exe and stale cached copies.
/// Every delete is best-effort — locked or missing files silently skipped.
fn gc_stale_files(my_exe: &Path) {
    // Clean .old.* files next to the exe.
    let parent = match my_exe.parent() {
        Some(p) => p,
        None => return,
    };
    let stem = match my_exe.file_name().and_then(|n| n.to_str()) {
        Some(s) => s.to_string(),
        None => return,
    };
    let entries = match fs::read_dir(parent) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(&stem) && name_str.contains(".old") {
            let _ = fs::remove_file(entry.path());
        }
    }

    // Clean stale cached copies in the global cache dir.
    let dir = cache_dir();
    if !dir.is_dir() {
        return;
    }
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let _ = fs::remove_file(entry.path());
    }
}

/// Rename ourselves out of the way, then copy a fresh unlocked copy back.
/// After this, the original path (Scripts/clud.exe) is an unlocked file
/// that pip can freely overwrite.
///
/// If any step fails, we silently continue — the app works either way.
fn unlock_self(my_exe: &Path) {
    let rand_id: u32 = std::process::id()
        ^ (std::time::UNIX_EPOCH
            .elapsed()
            .unwrap_or_default()
            .subsec_nanos());
    let old_exe = my_exe.with_extension(format!("exe.old.{rand_id}"));

    // Rename: clud.exe → clud.exe.old.<rand> (works on locked files on Windows).
    if fs::rename(my_exe, &old_exe).is_err() {
        return;
    }

    // Copy back: clud.exe.old.<rand> → clud.exe (new file, unlocked).
    // If this fails, the exe is gone — but we're already running from memory
    // so the current process is fine. Next pip install will recreate it.
    let _ = fs::copy(&old_exe, my_exe);
}

/// Cache directory: %LOCALAPPDATA%/clud/bin/ on Windows.
fn cache_dir() -> PathBuf {
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        PathBuf::from(local_app_data).join("clud").join("bin")
    } else {
        std::env::temp_dir().join("clud-bin")
    }
}

/// Fast hash of file contents.
fn hash_bytes(bytes: &[u8]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::Hasher;
    let mut hasher = DefaultHasher::new();
    hasher.write(bytes);
    format!("{:016x}", hasher.finish())
}

/// Remove cached copies that aren't the current one.
/// Every delete is best-effort — locked or missing files silently skipped.
fn cleanup_old_cached(dir: &Path, keep: &Path) -> u32 {
    let mut cleaned = 0u32;
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep {
            continue;
        }
        if fs::remove_file(&path).is_ok() {
            cleaned += 1;
        }
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_cleanup_old_cached() {
        let tmp = std::env::temp_dir().join("clud-trampoline-test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let keep = tmp.join("keep.exe");
        let old1 = tmp.join("old1.exe");
        let old2 = tmp.join("old2.exe");
        fs::write(&keep, b"keep").unwrap();
        fs::write(&old1, b"old1").unwrap();
        fs::write(&old2, b"old2").unwrap();

        let cleaned = cleanup_old_cached(&tmp, &keep);
        assert_eq!(cleaned, 2);
        assert!(keep.is_file());
        assert!(!old1.exists());
        assert!(!old2.exists());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_cleanup_missing_dir() {
        let missing = PathBuf::from("/nonexistent/dir/that/doesnt/exist");
        let keep = missing.join("keep.exe");
        // Should not panic — just returns 0.
        assert_eq!(cleanup_old_cached(&missing, &keep), 0);
    }

    #[test]
    fn test_gc_stale_missing_exe() {
        let missing = PathBuf::from("/nonexistent/clud.exe");
        // Should not panic.
        gc_stale_files(&missing);
    }
}
