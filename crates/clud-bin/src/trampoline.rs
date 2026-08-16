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

use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use crate::runtime_cache;

/// Run `program` with `args` as a **transparent relay child**: same stdio,
/// same cwd, same environment, no added process containment — then block
/// until it exits and return its exit code.
///
/// This is the spawn half of the runtime-cache hop
/// ([`runtime_cache::hop_to_runtime_cache_if_enabled`]). Windows has no
/// `execv`, so the hop cannot replace its own process image the way the Unix
/// branch does; it has to spawn the cached binary and wait. What it must not
/// do is *change* anything else about how that binary runs.
///
/// # Why raw `std::process::Command` and not `NativeProcess` (#333)
///
/// `NativeProcess::start` puts every Windows child in a fresh Job Object with
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and drops that job handle when the
/// wrapper goes away. For a relay that is not an implementation detail, it is
/// a behavior change with teeth: Job Object membership is *inherited*, so
/// everything the relayed clud starts joins the same job — including the
/// processes that are meant to outlive it. The `__daemon` started by
/// [`spawn_detached_self`] is exactly that. It detaches with
/// `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB`.
/// The breakaway keeps the daemon outside a client or test runner's
/// kill-on-close Job Object. If a supervisor forbids breakaway, the spawn
/// retries with the ordinary detached flags instead of failing a restart.
/// The observable symptom is a `daemon.json` naming a PID
/// that is already dead — the first of the 31 integration failures measured
/// with `CLUD_USE_RUNTIME_CACHE=1` on Windows and recorded in #333.
///
/// A relay must add nothing. This is the same reasoning that exempts
/// `bin/clud_shim.rs`, and it is why both files sit in `ci/banned_imports.py`'s
/// exempt set rather than routing through `running_process`.
///
/// Covered by `tests/runtime_cache_hop_windows.rs`.
pub fn relay_child_and_wait(program: &Path, args: &[impl AsRef<OsStr>]) -> std::io::Result<i32> {
    let mut command = std::process::Command::new(program);
    command.args(args);

    #[cfg(windows)]
    {
        // The cached child needs the caller's visible stdio, but its detached
        // descendants must not inherit the caller's pipe handles. Pass owned
        // inheritable duplicates explicitly and make the originals
        // non-inheritable for the CreateProcess call.
        let [stdin, stdout, stderr] = windows_stdio::duplicate_stdio_for_child()?;
        command.stdin(stdin).stdout(stdout).stderr(stderr);
        let _guard = windows_stdio::NonInheritableStdioGuard::install();
        let status = command.status()?;
        Ok(status.code().unwrap_or(1))
    }

    #[cfg(not(windows))]
    {
        let status = command.status()?;
        // A signalled child yields `None`; 1 is the conventional failed
        // stand-in. Production Unix hopping uses `execv`, but keeping this
        // helper total also makes its direct integration coverage portable.
        Ok(status.code().unwrap_or(1))
    }
}

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

    #[cfg(windows)]
    {
        // A caller can itself be contained in a job that forbids breakaway.
        // Prefer a durable daemon, but retain the prior detached behavior for
        // those supervisors instead of turning `daemon restart` into an
        // access-denied error.
        let _guard = windows_stdio::NonInheritableStdioGuard::install();
        match spawn_detached_command(&exe, args, true) {
            Ok(()) => return Ok(()),
            Err(err) if err.raw_os_error() == Some(5) => {
                return spawn_detached_command(&exe, args, false);
            }
            Err(err) => return Err(err),
        }
    }

    #[cfg(unix)]
    spawn_detached_command(&exe, args)
}

#[cfg(windows)]
fn spawn_detached_command(exe: &Path, args: &[String], break_away: bool) -> std::io::Result<()> {
    let mut command = std::process::Command::new(exe);
    command.args(args);
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::null());
    command.stderr(std::process::Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(detached_creation_flags(break_away));
    }

    let _child = command.spawn()?;
    Ok(())
}

#[cfg(windows)]
const fn detached_creation_flags(break_away: bool) -> u32 {
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    if break_away {
        flags | CREATE_BREAKAWAY_FROM_JOB
    } else {
        flags
    }
}

#[cfg(unix)]
fn spawn_detached_command(exe: &Path, args: &[String]) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;

    let mut command = std::process::Command::new(exe);
    command.args(args);
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::null());
    command.stderr(std::process::Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
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
    const DUPLICATE_SAME_ACCESS: u32 = 0x0000_0002;

    extern "system" {
        fn GetStdHandle(n_std_handle: u32) -> isize;
        fn GetHandleInformation(handle: isize, flags: *mut u32) -> i32;
        fn SetHandleInformation(handle: isize, mask: u32, flags: u32) -> i32;
        fn GetCurrentProcess() -> isize;
        fn DuplicateHandle(
            source_process_handle: isize,
            source_handle: isize,
            target_process_handle: isize,
            target_handle: *mut isize,
            desired_access: u32,
            inherit_handle: i32,
            options: u32,
        ) -> i32;
    }

    /// Duplicate the three standard handles for the relay's cached child.
    ///
    /// The copies deliberately start non-inheritable: `std::process::Command`
    /// creates the one inheritable copy it needs for each STARTUPINFO standard
    /// slot. Making these copies inheritable here would also leak them as
    /// ordinary handles to every descendant. See [`super::relay_child_and_wait`].
    pub(super) fn duplicate_stdio_for_child() -> std::io::Result<[std::process::Stdio; 3]> {
        use std::os::windows::io::FromRawHandle;

        let ids = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];
        let mut duplicated = Vec::with_capacity(ids.len());
        for std_id in ids {
            let source = unsafe { GetStdHandle(std_id) };
            if source == 0 || source == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "standard handle unavailable for runtime-cache relay",
                ));
            }

            let mut target = 0isize;
            let copied = unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    source,
                    GetCurrentProcess(),
                    &mut target,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            };
            if copied == 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: DuplicateHandle returned a fresh owned handle. Stdio
            // takes ownership through File and closes it after CreateProcess.
            let file = unsafe { std::fs::File::from_raw_handle(target as _) };
            duplicated.push(std::process::Stdio::from(file));
        }

        let mut handles = duplicated.into_iter();
        Ok([
            handles.next().expect("stdin duplicate exists"),
            handles.next().expect("stdout duplicate exists"),
            handles.next().expect("stderr duplicate exists"),
        ])
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
    if runtime_cache::exe_is_under_clud_runtime(&my_exe) {
        return;
    }

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

    #[cfg(windows)]
    #[test]
    fn detached_daemon_breaks_away_from_a_parent_job() {
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        assert_ne!(detached_creation_flags(true) & CREATE_BREAKAWAY_FROM_JOB, 0);
        assert_eq!(
            detached_creation_flags(false) & CREATE_BREAKAWAY_FROM_JOB,
            0
        );
    }

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
