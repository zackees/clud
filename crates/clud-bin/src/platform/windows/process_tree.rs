//! Windows half of `crate::process_tree`: console process-group signalling.

/// `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid)`; see
/// `crate::process_tree::try_break_group`.
pub fn try_break_group(pid: u32) -> bool {
    use windows::Win32::System::Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT};
    // SAFETY: `GenerateConsoleCtrlEvent` is documented as safe to
    // call with any PID; passing a non-existent group simply returns
    // FALSE without dereferencing memory. The function signature in
    // `windows-rs` is `unsafe extern "system"`, which is the standard
    // marker for Win32 entry points — no Rust invariant is violated.
    let ok = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) };
    ok.is_ok()
}

#[cfg(test)]
mod tests {
    use crate::process_identity::ProcessIdentity;
    use crate::process_tree::kill_tree;
    use std::time::Duration;
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

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

    /// Start time of `pid` if it is alive, from a fresh minimal snapshot.
    fn live_start_time(pid: u32) -> Option<u64> {
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        system.process(Pid::from_u32(pid)).map(|p| p.start_time())
    }

    /// #1738: Windows never rewrites a process's parent PID when the parent
    /// exits, and it recycles PIDs. A process whose parent died therefore
    /// looks like a child of whatever later process is handed that PID. A tree
    /// kill rooted at the recycled PID must not take the older process with it.
    ///
    /// The CI shape that hit this: some ancestor of the pytest runner has a
    /// dead parent; a clud daemon is later handed that PID; the daemon's tree
    /// kill then walks *up* into the runner and kills pytest.
    #[test]
    fn kill_tree_spares_older_process_whose_parent_pid_was_recycled() {
        use std::process::{Command, Stdio};

        // `start /b` makes ping.exe a child of this cmd.exe, which then exits
        // and leaves ping.exe with a parent PID that names a dead process.
        let mut launcher = Command::new("cmd")
            .args(["/c", "start", "", "/b", "ping", "-n", "120", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd /c start /b ping");
        let stale_parent = launcher.id();
        launcher.wait().expect("wait for launcher cmd.exe");
        // Close our handle: Windows keeps a PID reserved while any handle to
        // the dead process is open.
        drop(launcher);

        let mut orphan = None;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while orphan.is_none() && std::time::Instant::now() < deadline {
            let mut system = System::new();
            system.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing(),
            );
            orphan = system.processes().iter().find_map(|(pid, process)| {
                (process.parent() == Some(Pid::from_u32(stale_parent))
                    && process.name().eq_ignore_ascii_case("ping.exe"))
                .then(|| ProcessIdentity::new(pid.as_u32(), process.start_time()))
            });
            if orphan.is_none() {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let orphan = orphan.expect("ping.exe launched by `start /b` never appeared");

        // Recycle the dead parent's PID onto a fresh process. Non-matching
        // candidates are released at once; Windows hands freed PIDs back out.
        let mut holder = None;
        let mut attempts = 0u32;
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            attempts += 1;
            let mut candidate = Command::new("ping")
                .args(["-n", "120", "127.0.0.1"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn recycling candidate");
            if candidate.id() == stale_parent {
                holder = Some(candidate);
                break;
            }
            let _ = candidate.kill();
            let _ = candidate.wait();
        }
        let Some(mut holder) = holder else {
            kill_tree(orphan.pid);
            eprintln!("inconclusive: PID {stale_parent} was not recycled after {attempts} spawns");
            return;
        };
        eprintln!("PID {stale_parent} recycled after {attempts} spawns");

        kill_tree(holder.id());
        std::thread::sleep(Duration::from_millis(500));
        let survived = live_start_time(orphan.pid) == Some(orphan.start_time);

        let _ = holder.kill();
        let _ = holder.wait();
        if survived {
            kill_tree(orphan.pid);
        }
        assert!(
            survived,
            "kill_tree({stale_parent}) killed PID {} (started {}), which predates the root \
             and only names it as parent because Windows recycled the PID",
            orphan.pid, orphan.start_time
        );
    }
}
