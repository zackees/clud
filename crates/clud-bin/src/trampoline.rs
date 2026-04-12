//! Windows trampoline: ensures pip can always overwrite clud.exe.
//!
//! Problem: On Windows, running executables are file-locked. `pip install .`
//! fails if clud is running because it can't overwrite Scripts/clud.exe.
//!
//! Solution: On launch, clud renames itself (Scripts/clud.exe → clud.exe.old),
//! copies a fresh unlocked copy back (clud.exe.old → clud.exe), then runs
//! the real work from a cached copy in %LOCALAPPDATA%/clud/bin/<hash>.exe.
//!
//! Result: Scripts/clud.exe is NEVER locked. pip install always works.
//!
//! On Linux/macOS: no-op (Unix allows deleting running binaries).

use std::fs;
use std::path::{Path, PathBuf};

/// Env var set on the cached copy so it knows it's already trampolined.
const TRAMPOLINE_ENV: &str = "_CLUD_TRAMPOLINED";

/// Try to trampoline. Returns `Some(exit_code)` if we spawned the cached
/// copy (caller should exit). Returns `None` if we ARE the cached copy
/// or trampolining is not needed (Unix).
pub fn maybe_trampoline() -> Option<i32> {
    if !cfg!(target_os = "windows") {
        return None;
    }

    // Already the cached copy — run normally.
    if std::env::var(TRAMPOLINE_ENV).is_ok() {
        return None;
    }

    let my_exe = std::env::current_exe().ok()?;

    // Step 1: GC stale .old and cached files in the background (don't block startup).
    let gc_exe = my_exe.clone();
    std::thread::spawn(move || gc_stale_files(&gc_exe));

    // Step 2: Rename ourselves so Scripts/clud.exe becomes unlocked.
    unlock_self(&my_exe);

    // Step 3: Copy to global cache and spawn from there.
    let my_bytes = fs::read(&my_exe).ok()?;
    let hash = hash_bytes(&my_bytes);
    let cache_dir = cache_dir();
    fs::create_dir_all(&cache_dir).ok()?;

    let cached_exe = cache_dir.join(format!("{hash}.exe"));
    if !cached_exe.is_file() {
        if let Err(e) = fs::write(&cached_exe, &my_bytes) {
            eprintln!("[clud] trampoline: failed to cache binary: {e}");
            return None;
        }
    }

    // Step 4: Clean up old cached copies (best-effort).
    cleanup_old_cached(&cache_dir, &cached_exe);

    // Step 4: Spawn the cached copy with all args, wait for it.
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
            None
        }
    }
}

/// GC stale .old files next to the exe and stale cached copies.
/// Runs before anything else so we clean up from previous launches.
fn gc_stale_files(my_exe: &Path) {
    // Clean .old / .old.1 / .old.2 / etc. next to the exe.
    if let Some(parent) = my_exe.parent() {
        if let Some(stem) = my_exe.file_name().and_then(|n| n.to_str()) {
            if let Ok(entries) = fs::read_dir(parent) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str.starts_with(stem) && name_str.contains(".old") {
                        // Try to delete — silently skip if still locked.
                        let _ = fs::remove_file(entry.path());
                    }
                }
            }
        }
    }

    // Clean stale cached copies in the global cache dir.
    let dir = cache_dir();
    if dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                // Try to delete everything — the current hash will be
                // re-created moments later. Locked files silently skipped.
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// Rename ourselves out of the way, then copy a fresh unlocked copy back.
/// After this, the original path (Scripts/clud.exe) is an unlocked file
/// that pip can freely overwrite.
fn unlock_self(my_exe: &Path) {
    // Find an available .old slot. If .old is locked (previous instance
    // still running), stack to .old.1, .old.2, etc.
    let old_exe = find_old_slot(my_exe);

    // Rename: clud.exe → clud.exe.old (works on locked files on Windows).
    if fs::rename(my_exe, &old_exe).is_err() {
        return; // Can't rename — maybe already handled, continue anyway.
    }

    // Copy back: clud.exe.old → clud.exe (new file, unlocked).
    let _ = fs::copy(&old_exe, my_exe);
}

/// Find an available .old filename. Tries clud.exe.old, then .old.1, .old.2, etc.
fn find_old_slot(my_exe: &Path) -> PathBuf {
    let base = my_exe.with_extension("exe.old");
    if !base.exists() || fs::remove_file(&base).is_ok() {
        return base;
    }
    // .old exists and is locked — stack.
    for i in 1..100 {
        let candidate = my_exe.with_extension(format!("exe.old.{i}"));
        if !candidate.exists() || fs::remove_file(&candidate).is_ok() {
            return candidate;
        }
    }
    // Worst case: reuse the base name (rename will fail, we'll fall through).
    base
}

/// Cache directory: %LOCALAPPDATA%/clud/bin/ on Windows.
fn cache_dir() -> PathBuf {
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        PathBuf::from(local_app_data).join("clud").join("bin")
    } else {
        // Fallback to temp dir.
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
/// Silently skips locked files (still running).
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

        let cleaned = cleanup_old_cached(&tmp, &keep);
        assert_eq!(cleaned, 2);
        assert!(keep.is_file());
        assert!(!old1.exists());
        assert!(!old2.exists());

        let _ = fs::remove_dir_all(&tmp);
    }
}
