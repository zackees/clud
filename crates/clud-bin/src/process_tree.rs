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
//! When the user hits Ctrl+C, `process.kill()` on a
//! `running_process_core::NativeProcess` only terminates the **direct**
//! child (cmd.exe). The orphaned `node.exe` keeps writing to the inherited
//! console for several seconds until clud itself exits and its Job Object
//! closes — that's the multi-second hang users were reporting.
//!
//! The fix is to walk the descendant tree before reaping the direct child.
//! This module provides [`kill_tree`] for that. It mirrors the
//! `signal_process_tree` helper already used by [`crate::daemon`]: scan
//! the process table with `sysinfo`, walk parent→children, and SIGKILL
//! (or Windows `TerminateProcess`) every descendant before the root.
//!
//! Best-effort: failures are silent and the whole operation is bounded by
//! the cost of one `sysinfo` system snapshot, which is well under our
//! sub-second Ctrl+C latency target.

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System};

/// Kill the process tree rooted at `pid`, including the root itself.
///
/// Best-effort and cross-platform. Uses `sysinfo` to enumerate descendants
/// (the same approach already in [`crate::daemon::signal_process_tree`])
/// so we don't need to shell out to OS helpers like `taskkill` or `pgrep`.
///
/// We refresh with `ProcessRefreshKind::nothing()` — we only need the
/// parent-PID graph, not CPU/memory/cmdline. On Windows in particular,
/// `System::new_all()` enumerates every process's full metadata and takes
/// tens of seconds; the minimal refresh is sub-second, which is the budget
/// we have on the Ctrl+C path.
pub fn kill_tree(pid: u32) {
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let root = Pid::from_u32(pid);
    if system.process(root).is_none() {
        // Already dead, or never existed. Nothing to do.
        return;
    }

    // Kill leaves first, root last. `descendants` is BFS order
    // (root's children, then grandchildren, ...); reversing gets us
    // deepest-first.
    let mut descendants = descendant_pids(&system, root);
    descendants.reverse();
    descendants.push(root);

    for descendant in descendants {
        if let Some(process) = system.process(descendant) {
            // `kill_with(Signal::Kill)` is SIGKILL on Unix. On Windows it
            // returns `None` (signals aren't a Windows concept), so we
            // always follow up with `process.kill()` which is
            // `TerminateProcess` on Windows and a no-op redundant SIGKILL
            // on Unix.
            let _ = process.kill_with(Signal::Kill);
            let _ = process.kill();
        }
    }
}

fn descendant_pids(system: &System, root: Pid) -> Vec<Pid> {
    let mut children: std::collections::HashMap<Pid, Vec<Pid>> = std::collections::HashMap::new();
    for (pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            children.entry(parent).or_default().push(*pid);
        }
    }
    let mut stack = vec![root];
    let mut descendants = Vec::new();
    while let Some(current) = stack.pop() {
        if let Some(next) = children.get(&current) {
            for child in next {
                descendants.push(*child);
                stack.push(*child);
            }
        }
    }
    descendants
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn kill_tree_of_dead_pid_does_not_panic() {
        // A PID that almost certainly doesn't exist: u32::MAX. The helper
        // must return promptly without panicking — the whole point of the
        // "best-effort" contract is that nothing on the Ctrl+C path can
        // throw.
        let start = std::time::Instant::now();
        kill_tree(u32::MAX);
        // One `System::new_all()` snapshot dominates the wall clock; even
        // on slow CI we expect well under 2s.
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "kill_tree on dead pid took too long: {:?}",
            start.elapsed()
        );
    }

    #[cfg(windows)]
    #[test]
    fn kill_tree_terminates_real_descendant_on_windows() {
        // Spawn `cmd /c timeout 30`. That creates a child cmd.exe which
        // itself spawns timeout.exe — mirroring the real `clud → cmd.exe
        // → node.exe` tree shape. Then call `kill_tree` on the cmd.exe
        // PID and assert it dies within 5s.
        //
        // `std::process::Command` is exempt from the banned-imports rule
        // only inside tests in this module; production code paths must
        // still go through `running-process-core`.
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "timeout", "/t", "30", "/nobreak"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn cmd /c timeout");
        let pid = child.id();

        // Give cmd.exe a moment to spawn its timeout.exe grandchild.
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
        // Spawn `sh -c 'sleep 30'`. The shell is the parent of `sleep`,
        // so killing the tree must SIGKILL both. We check the sh process
        // is reaped within 5s.
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 30"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sh -c sleep 30");
        let pid = child.id();

        // Let sh spawn its sleep grandchild.
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
