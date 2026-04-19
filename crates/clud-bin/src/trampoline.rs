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

/// Spawn the current executable as a detached background process.
///
/// On Windows, takes care to prevent the detached child from inheriting our
/// parent's stdio pipe handles. Rust's `std::process::Command` always calls
/// `CreateProcess` with `bInheritHandles=TRUE` when stdio is redirected;
/// that copies *every* inheritable handle in our process into the child's
/// handle table, including the stdout/stderr pipe write-ends we inherited
/// from a test harness or supervisor. The child ignores them — its stdio
/// is `Stdio::null()` — but those handles stay in its handle table for its
/// entire lifetime, so the pipe's writer ref-count never drops to zero and
/// the reader (e.g. Python `subprocess.communicate`) never sees EOF.
///
/// The fix: clear `HANDLE_FLAG_INHERIT` on our three stdio handles around
/// the `CreateProcess` call. `Stdio::null()` uses a separate code path
/// (the STARTUPINFO `hStd*` fields) so NUL still reaches the child as its
/// actual stdin/stdout/stderr, but no *other* handle transfers. This was
/// the root cause of the 45-minute Windows integration-test cancellation
/// investigated in #37 and the PTY attach timeouts in #38.
pub fn spawn_detached_self(args: &[String]) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut command = std::process::Command::new(exe);
    command.args(args);
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::null());
    command.stderr(std::process::Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    #[cfg(windows)]
    let _guard = windows_stdio::NonInheritableStdioGuard::install();
    let _child = command.spawn()?;
    Ok(())
}

#[cfg(windows)]
mod windows_stdio {
    //! RAII guard that strips `HANDLE_FLAG_INHERIT` from our three standard
    //! handles for the lifetime of the guard, restoring the original flags
    //! on drop. Used to bracket detached-child spawns so the child doesn't
    //! inherit parent stdio pipes — see the module doc of the parent file.

    const HANDLE_FLAG_INHERIT: u32 = 0x0001;
    // Windows STD_*_HANDLE values are `((DWORD)-N)`; in Rust const context
    // the `as u32` cast on a negative i32 produces the matching bit pattern.
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const INVALID_HANDLE_VALUE: isize = -1;

    extern "system" {
        fn GetStdHandle(n_std_handle: u32) -> isize;
        fn GetHandleInformation(handle: isize, flags: *mut u32) -> i32;
        fn SetHandleInformation(handle: isize, mask: u32, flags: u32) -> i32;
    }

    pub(super) struct NonInheritableStdioGuard {
        saved: [Option<(isize, u32)>; 3],
    }

    impl NonInheritableStdioGuard {
        pub(super) fn install() -> Self {
            let ids = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];
            let mut saved: [Option<(isize, u32)>; 3] = [None, None, None];
            for (i, std_id) in ids.iter().enumerate() {
                unsafe {
                    let h = GetStdHandle(*std_id);
                    if h == 0 || h == INVALID_HANDLE_VALUE {
                        continue;
                    }
                    let mut flags = 0u32;
                    if GetHandleInformation(h, &mut flags) == 0 {
                        continue;
                    }
                    if flags & HANDLE_FLAG_INHERIT == 0 {
                        // Already non-inheritable; nothing to do.
                        continue;
                    }
                    if SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0) != 0 {
                        saved[i] = Some((h, flags));
                    }
                }
            }
            Self { saved }
        }
    }

    impl Drop for NonInheritableStdioGuard {
        fn drop(&mut self) {
            for item in &self.saved {
                if let Some((h, flags)) = *item {
                    unsafe {
                        SetHandleInformation(h, HANDLE_FLAG_INHERIT, flags & HANDLE_FLAG_INHERIT);
                    }
                }
            }
        }
    }
}

/// Unlock ourselves so pip can overwrite clud.exe while we're running.
/// Call this at the very start of main(), before any real work.
pub fn unlock_exe() {
    if !cfg!(target_os = "windows") {
        return;
    }

    // Escape hatch for CI / test harnesses that spawn many short-lived clud
    // invocations: the rename+copy+GC dance on every start costs real time
    // and, under investigation in #37, appears to keep stdout/stderr pipe
    // handles open on Windows GHA runners so Python's subprocess.run never
    // sees EOF. Set `CLUD_NO_UNLOCK=1` to disable.
    if std::env::var_os("CLUD_NO_UNLOCK").is_some() {
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
