//! Windows exe unlock: ensures pip can always overwrite clud.exe.
//!
//! Problem: On Windows, running executables are file-locked. `pip install .`
//! fails if clud is running because it can't overwrite Scripts/clud.exe.
//!
//! Solution: On launch, clud renames itself (Scripts/clud.exe → clud.exe.old.<rand>),
//! then copies a fresh unlocked copy back to Scripts/clud.exe. The running process
//! continues from the renamed file. No child process, no handle transfer.
//!
//! Result: Scripts/clud.exe is always an unlocked copy. pip install always works.
//! Each running instance locks its own clud.exe.old.<rand> file.
//!
//! IMPORTANT: Every operation is best-effort. If anything fails, the app
//! continues normally — it just won't get the lock-free install benefit.
//!
//! On Linux/macOS: no-op (Unix allows deleting running binaries).

use std::fs;
use std::path::Path;

/// Unlock ourselves so pip can overwrite clud.exe while we're running.
/// Call this at the very start of main(), before any real work.
pub fn unlock_exe() {
    if !cfg!(target_os = "windows") {
        return;
    }

    let my_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };

    // Rename clud.exe → clud.exe.old.<rand>. We keep running from the renamed file.
    let rand_id: u32 = std::process::id()
        ^ (std::time::UNIX_EPOCH
            .elapsed()
            .unwrap_or_default()
            .subsec_nanos());
    let old_exe = my_exe.with_extension(format!("exe.old.{rand_id}"));

    if fs::rename(&my_exe, &old_exe).is_err() {
        eprintln!("[clud] warning: could not unlock exe for hot-reload. pip install may fail while clud is running.");
        return;
    }

    // Copy back: clud.exe.old.<rand> → clud.exe (new file, unlocked).
    let _ = fs::copy(&old_exe, &my_exe);

    // GC stale .old files in background. Fire and forget.
    let parent = match my_exe.parent() {
        Some(p) => p.to_path_buf(),
        None => return,
    };
    let stem = match my_exe.file_name().and_then(|n| n.to_str()) {
        Some(s) => s.to_string(),
        None => return,
    };
    std::thread::spawn(move || gc_old_files(&parent, &stem));
}

/// Delete stale .old files next to the exe. Best-effort — locked files skipped.
fn gc_old_files(dir: &Path, stem: &str) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(stem) && name_str.contains(".old") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gc_old_files() {
        let tmp = std::env::temp_dir().join("clud-unlock-test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // Simulate: clud.exe + two stale .old files
        fs::write(tmp.join("clud.exe"), b"current").unwrap();
        fs::write(tmp.join("clud.exe.old.111"), b"old1").unwrap();
        fs::write(tmp.join("clud.exe.old.222"), b"old2").unwrap();
        fs::write(tmp.join("other.exe"), b"unrelated").unwrap();

        gc_old_files(&tmp, "clud.exe");

        assert!(tmp.join("clud.exe").is_file()); // untouched
        assert!(!tmp.join("clud.exe.old.111").exists()); // cleaned
        assert!(!tmp.join("clud.exe.old.222").exists()); // cleaned
        assert!(tmp.join("other.exe").is_file()); // untouched

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_gc_missing_dir() {
        // Should not panic on nonexistent directory.
        gc_old_files(Path::new("/nonexistent/dir"), "clud.exe");
    }
}
