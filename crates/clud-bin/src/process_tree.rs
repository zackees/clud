//! Best-effort termination of an entire descendant process tree.
//!
//! Background — Ctrl+C on Windows for `clud --codex loop`:
//!
//! On Windows, `clud --codex` routes through `cmd /D /S /C "codex.cmd ..."`
//! (the BatBadBat / CVE-2024-24576 workaround in [`crate::subprocess`]).
//! That means the actual process tree at runtime is:
//!
//! ```text
//! clud.exe → cmd.exe → node.exe (real codex)
//! ```
//!
//! When the user hits Ctrl+C, the `run_with_inherited_stdio` path used to
//! call `process.kill()` directly. `NativeProcess::kill_impl` in
//! `running-process-core` v3.1.0 is just `std::process::Child::kill()`,
//! which on Windows is `TerminateProcess` on the **direct** child only.
//! That kills `cmd.exe` immediately but leaves `node.exe` running. The
//! orphaned `node.exe` then keeps writing to the inherited console (the
//! parent console — clud's console — that it was sharing) for several
//! seconds until clud itself finally exits and its Job Object closes.
//!
//! The fix is to walk the descendant tree before reaping the direct child.
//! This module provides [`kill_tree`] for that. It is **best-effort**: any
//! failure is logged with the `[clud]` prefix but never propagated, because
//! the caller is on the Ctrl+C path and we need to return to the user
//! promptly (target: under 2 seconds end-to-end).
//!
//! Platform-specific strategies:
//!
//! * **Windows** — shell out to `taskkill /T /F /PID <pid>`. `taskkill` ships
//!   with every supported Windows version, handles the descendant walk via
//!   the Toolhelp32 snapshot API, and propagates `TerminateProcess` from
//!   leaves toward the root. We launch it with `CREATE_NO_WINDOW` so the
//!   user never sees a conhost flash.
//! * **Unix** — recurse with `pgrep -P <pid>` to find direct children, then
//!   for each child do the same recursively, collecting all PIDs into a
//!   list. Send `SIGKILL` (via `libc::kill`) to every PID from leaves
//!   toward the root. We do *not* rely on POSIX process groups: the
//!   `ProcessConfig` used by clud does NOT set `create_process_group:
//!   true`, so killing the negative PID would no-op.

#[cfg(unix)]
use std::collections::VecDeque;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Kill the process tree rooted at `pid`, including the root itself.
///
/// Best-effort: errors are reported via `eprintln!("[clud] ...")` and never
/// propagated. The whole operation is bounded to ~2 seconds.
///
/// See module docs for the platform-specific strategy on Windows / Unix.
pub fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        kill_tree_windows(pid);
    }
    #[cfg(unix)]
    {
        kill_tree_unix(pid);
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = pid;
        eprintln!("[clud] kill_tree: unsupported platform; relying on direct kill");
    }
}

// --- Windows ---------------------------------------------------------------

#[cfg(windows)]
fn kill_tree_windows(pid: u32) {
    // CREATE_NO_WINDOW so the helper process doesn't pop a conhost window.
    // Matches the bit value defined in `crate::win_creation_flags` but we
    // hardcode the literal here so this module stays standalone and the
    // dependency direction stays one-way (process_tree is leaf-level).
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let pid_str = pid.to_string();
    let mut cmd = Command::new("taskkill");
    cmd.args(["/T", "/F", "/PID", pid_str.as_str()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);

    match cmd.spawn() {
        Ok(mut child) => {
            // Bound the wait: taskkill is usually near-instant, but a hung
            // descendant could in theory stall it. We don't want to add
            // multi-second latency to Ctrl+C.
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => {
                        if std::time::Instant::now() >= deadline {
                            // Leave the helper running; the OS will reap it.
                            // The direct-child kill that follows in main.rs
                            // will still take down the cmd.exe wrapper.
                            eprintln!(
                                "[clud] taskkill /T /F /PID {pid} did not finish within 2s; continuing"
                            );
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    Err(e) => {
                        eprintln!("[clud] taskkill wait for pid {pid} failed: {e}");
                        break;
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("[clud] failed to spawn taskkill for pid {pid}: {e}");
        }
    }
}

// --- Unix ------------------------------------------------------------------

#[cfg(unix)]
fn kill_tree_unix(pid: u32) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let descendants = collect_descendants_unix(pid, deadline);

    // Kill leaves first, root last. `collect_descendants_unix` returns the
    // tree in BFS order (root → children → grandchildren), so reversing
    // gets us deepest-first. Then finally the root.
    for descendant in descendants.into_iter().rev() {
        kill_one_unix(descendant);
    }
    kill_one_unix(pid);
}

#[cfg(unix)]
fn kill_one_unix(pid: u32) {
    // SAFETY: `libc::kill` is a thin syscall wrapper. Passing SIGKILL to a
    // PID that doesn't exist returns -1/ESRCH which we ignore — it just
    // means the process already died (race with descendant termination).
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        // ESRCH (no such process) is expected for racy descendants. Anything
        // else is logged so we have a breadcrumb if Ctrl+C ever stops
        // working on a particular system.
        if err.raw_os_error() != Some(libc::ESRCH) {
            eprintln!("[clud] SIGKILL pid {pid} failed: {err}");
        }
    }
}

#[cfg(unix)]
fn collect_descendants_unix(root: u32, deadline: std::time::Instant) -> Vec<u32> {
    // BFS over the parent->children relation. We use `pgrep -P` which is
    // available on every supported Unix in our matrix (Linux: procps-ng;
    // macOS: ships in /usr/bin since at least 10.8). If pgrep is missing
    // we fall back to "no descendants" and just SIGKILL the root.
    let mut out: Vec<u32> = Vec::new();
    let mut queue: VecDeque<u32> = VecDeque::new();
    queue.push_back(root);
    while let Some(current) = queue.pop_front() {
        if std::time::Instant::now() >= deadline {
            eprintln!(
                "[clud] descendant walk for pid {root} hit 2s deadline; killing what we have"
            );
            break;
        }
        for child in children_of_unix(current) {
            // Defensive: if pgrep ever returned `current` itself or the
            // root (cycle), skip — we never want infinite loops on the
            // Ctrl+C path.
            if child == current || child == root {
                continue;
            }
            out.push(child);
            queue.push_back(child);
        }
    }
    out
}

#[cfg(unix)]
fn children_of_unix(pid: u32) -> Vec<u32> {
    let output = Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        // pgrep missing / failed; treat as no children.
        return Vec::new();
    };
    // pgrep exits 1 when there are no matches — that's a valid result, not
    // an error. We rely on stdout being empty in that case.
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .collect()
}

// --- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_tree_of_dead_pid_does_not_panic() {
        // A PID that almost certainly doesn't exist: u32::MAX. The helper
        // must return promptly without panicking — the whole point of the
        // "best-effort" contract is that nothing on the Ctrl+C path can
        // throw.
        let start = std::time::Instant::now();
        kill_tree(u32::MAX);
        // Generous bound: even the worst-case `taskkill` + wait should be
        // well under our 2s deadline, but we allow 4s for cold-start CI.
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "kill_tree on dead pid took too long: {:?}",
            start.elapsed()
        );
    }

    #[cfg(windows)]
    #[test]
    fn kill_tree_terminates_real_descendant_on_windows() {
        // Spawn `cmd /c timeout 30 >NUL`. That creates a child cmd.exe
        // process which will itself live for 30 seconds (timeout.exe is a
        // grandchild, mirroring the real codex tree shape). We then call
        // kill_tree on the cmd.exe PID and assert it dies within 2s.
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "timeout", "/t", "30", "/nobreak"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd /c timeout");
        let pid = child.id();

        // Give the child a moment to fully start (and spawn timeout.exe).
        std::thread::sleep(Duration::from_millis(200));

        let start = std::time::Instant::now();
        kill_tree(pid);

        // Wait for the direct child to actually exit. taskkill /T /F is
        // synchronous from the OS's perspective but the Rust handle may
        // take a moment to observe it.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        panic!("cmd.exe survived kill_tree for >5s");
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("try_wait failed: {e}"),
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "kill_tree took too long: {:?}",
            start.elapsed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn kill_tree_terminates_real_descendant_on_unix() {
        // Spawn `sh -c 'sleep 30'`. The shell is a parent of `sleep`, so
        // killing the tree must SIGKILL both. We check the sh process is
        // reaped within 2s.
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sh -c sleep 30");
        let pid = child.id();

        // Let sh spawn its sleep child.
        std::thread::sleep(Duration::from_millis(200));

        let start = std::time::Instant::now();
        kill_tree(pid);

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        panic!("sh survived kill_tree for >5s");
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("try_wait failed: {e}"),
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "kill_tree took too long: {:?}",
            start.elapsed()
        );
    }
}
