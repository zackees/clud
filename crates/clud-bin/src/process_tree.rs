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
//! `running_process::NativeProcess` only terminates the **direct**
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
//!
//! [`try_break_group`] is the cooperative companion on Windows: it sends
//! `CTRL_BREAK_EVENT` to the child's console process group so a
//! well-behaved agent can flush state before the hard `kill_tree` follows.

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
    kill_tree_filtered(pid, &|_| true);
}

/// Kill the tree rooted at `pid`, consulting `may_kill` for every process.
///
/// `may_kill(pid) == false` **prunes**: that process is spared *and so is its
/// entire subtree*. Sparing a daemon while killing its children would leave
/// it wedged mid-work, which is worse than either extreme — a build daemon's
/// compiler children are its in-flight work, not leaked garbage.
///
/// # Why a caller-supplied predicate instead of a policy baked in here
///
/// The two callers want opposite things. Automatic reapers (shell exit,
/// orphan sweep) must spare declared daemons — a `zccache`/`soldr`/`fbuild`
/// server started by an agent bash command is not leaked garbage, and killing
/// it throws away a warm cache shared with every other session. Deliberate
/// kills (`clud kill`, `clud slay`, Ctrl+C) mean *everything*, and pass the
/// permissive predicate via [`kill_tree`].
///
/// # Why the predicate takes only a PID
///
/// This runs on the Ctrl+C path, so the snapshot is deliberately built with
/// `ProcessRefreshKind::nothing()` (see the note on [`kill_tree`]) — no
/// cmdline, no environment. A predicate needing richer facts must precompute
/// them and close over the result; [`crate::orphan_reaper`] already has the
/// originator-tagged PID set in hand and closes over that. Keeping the
/// predicate a pure PID lookup is what preserves the sub-second budget.
///
/// Note this is deliberately *not* a name allowlist. "Is this a daemon?" is
/// answered by whether the process carries the inherited
/// `RUNNING_PROCESS_ORIGINATOR` tag: everything an agent spawns inherits it
/// transitively, and a process that spawned itself as a daemon has stripped
/// it. Matching on image names would misfire on every unrelated build.
pub fn kill_tree_filtered(pid: u32, may_kill: &dyn Fn(u32) -> bool) {
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let root = Pid::from_u32(pid);
    if system.process(root).is_none() {
        // Already dead, or never existed. Nothing to do.
        return;
    }
    if !may_kill(pid) {
        // Root is exempt — the whole tree is pruned.
        return;
    }

    // Kill leaves first, root last. `descendants` is BFS order
    // (root's children, then grandchildren, ...); reversing gets us
    // deepest-first.
    let mut descendants = descendant_pids_filtered(&system, root, may_kill);
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

/// BFS the parent→child graph from `root`, pruning any subtree whose root
/// `may_kill` rejects.
fn descendant_pids_filtered(
    system: &System,
    root: Pid,
    may_kill: &dyn Fn(u32) -> bool,
) -> Vec<Pid> {
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
                // Pruned, not just skipped: an exempt process keeps its own
                // descendants, so we never descend past it.
                if !may_kill(child.as_u32()) {
                    continue;
                }
                descendants.push(*child);
                stack.push(*child);
            }
        }
    }
    descendants
}

/// Whether Ctrl+C teardown should start with a cooperative Ctrl+Break.
///
/// Native backend executables can receive this best-effort signal before the
/// hard kill. Windows batch wrappers cannot: their direct child is `cmd.exe`,
/// and Ctrl+Break makes cmd's batch interpreter print `Terminate batch job
/// (Y/N)?` and wait on stdin. For those wrappers we skip straight to
/// `kill_tree`, which still terminates the cmd.exe child and its descendants.
pub fn should_cooperative_break(direct_child_is_batch_wrapper: bool) -> bool {
    !direct_child_is_batch_wrapper
}

/// Best-effort cooperative shutdown of a Windows console process group.
///
/// When clud spawns the backend with `CREATE_NEW_PROCESS_GROUP`, the
/// child becomes the root of a new console process group identified by
/// its PID. Calling `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid)`
/// delivers a break signal to every process in that group, giving a
/// well-behaved agent (one that installs a `SetConsoleCtrlHandler` for
/// `CTRL_BREAK_EVENT`) a chance to flush state before clud follows up
/// with a hard `kill_tree`.
///
/// Returns `true` if the OS accepted the call (the group existed and the
/// event was queued), `false` otherwise. Failures are silent and
/// non-fatal: the caller is expected to fall through to `kill_tree` after
/// a short grace window regardless.
///
/// Do not call this when the direct child is a `cmd.exe` batch wrapper; see
/// [`should_cooperative_break`] for the user-visible prompt it avoids.
///
/// No-op on non-Windows targets — POSIX has no `CREATE_NEW_PROCESS_GROUP`
/// concept and clud's foreground process group already receives the
/// terminal's SIGINT directly.
pub fn try_break_group(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::System::Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT};
        // SAFETY: `GenerateConsoleCtrlEvent` is documented as safe to
        // call with any PID; passing a non-existent group simply returns
        // FALSE without dereferencing memory. The function signature in
        // `windows-rs` is `unsafe extern "system"`, which is the standard
        // marker for Win32 entry points — no Rust invariant is violated.
        let ok = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) };
        ok.is_ok()
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod filter_tests {
    use std::collections::HashMap;

    /// Build a fake parent→children graph and run the same prune walk
    /// `descendant_pids_filtered` performs, without touching real processes.
    fn walk(edges: &[(u32, u32)], root: u32, may_kill: &dyn Fn(u32) -> bool) -> Vec<u32> {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for (parent, child) in edges {
            children.entry(*parent).or_default().push(*child);
        }
        let mut stack = vec![root];
        let mut out = Vec::new();
        while let Some(current) = stack.pop() {
            if let Some(next) = children.get(&current) {
                for child in next {
                    if !may_kill(*child) {
                        continue;
                    }
                    out.push(*child);
                    stack.push(*child);
                }
            }
        }
        out.sort_unstable();
        out
    }

    // shell(10) → bash(11) → cargo(12) → zccache-daemon(13) → compiler(14)
    const EDGES: &[(u32, u32)] = &[(10, 11), (11, 12), (12, 13), (13, 14)];

    #[test]
    fn permissive_filter_reaps_the_whole_tree() {
        assert_eq!(walk(EDGES, 10, &|_| true), vec![11, 12, 13, 14]);
    }

    /// The daemon is spared AND so is the compiler child beneath it —
    /// sparing the daemon but killing its in-flight work would leave it
    /// wedged, which is worse than either extreme.
    #[test]
    fn exempt_process_prunes_its_entire_subtree() {
        let tagged = |pid: u32| pid != 13;
        assert_eq!(walk(EDGES, 10, &tagged), vec![11, 12]);
    }

    /// An exempt process deeper in the tree must not spare its ancestors —
    /// the leaked bash/cargo above it are still garbage.
    #[test]
    fn exemption_does_not_propagate_upward() {
        let out = walk(EDGES, 10, &|pid| pid != 14);
        assert!(out.contains(&13), "daemon's parent still reaped: {out:?}");
        assert!(!out.contains(&14));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn cooperative_break_skipped_for_batch_wrapper() {
        assert!(!super::should_cooperative_break(true));
    }

    #[test]
    fn cooperative_break_sent_for_native_executable() {
        assert!(super::should_cooperative_break(false));
    }

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
