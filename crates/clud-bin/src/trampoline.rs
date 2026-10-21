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
/// `daemon::client::spawn_detached_daemon` is exactly that. Since #1186 it is
/// launched through running-process's daemon spawn with
/// `CREATE_BREAKAWAY_FROM_JOB`, which keeps it outside a client or test
/// runner's kill-on-close Job Object; if a supervisor forbids breakaway, the
/// spawn retries without it instead of failing a restart. A relay that added
/// its own job would still capture everything the relayed clud starts first.
/// The observable symptom is a `daemon.json` naming a PID
/// that is already dead — the first of the 31 integration failures measured
/// with `CLUD_USE_RUNTIME_CACHE=1` on Windows and recorded in #333.
///
/// A relay must add nothing. This is the same reasoning that exempts
/// `shim_main.rs`, and it is why both files sit in `ci/banned_imports.py`'s
/// exempt set rather than routing through `running_process`.
///
/// Covered by `tests/integration/diagnostics/platform/windows/runtime_cache_hop_windows.rs`.
pub fn relay_child_and_wait(program: &Path, args: &[impl AsRef<OsStr>]) -> std::io::Result<i32> {
    let mut command = std::process::Command::new(program);
    command.args(args);

    crate::platform::trampoline::relay_status(&mut command)
}

/// Unlock ourselves so pip can overwrite clud.exe while we're running.
/// Call this at the very start of main(), before any real work.
pub fn unlock_exe() {
    if !crate::platform::host::WINDOWS {
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
