use running_process::pty::NativePtyProcess;

use crate::verbose_log;

pub(super) fn reap_pty_exit(process: &NativePtyProcess) -> i32 {
    process.wait_impl(Some(1.0)).unwrap_or(1)
}

/// Escalate the `interrupted` flag to a real child-kill. Called once the
/// pump has observed the flag. Platform-split because `send_interrupt_impl`
/// is a byte-write on Windows (duplicates the 0x03 already forwarded via
/// raw-mode stdin) and a pgroup-SIGINT on POSIX (cooperative, no duplicate).
///
/// Both branches now try a fire-and-forget daemon handoff first so the
/// CLI can return to the shell in sub-100ms instead of blocking on
/// `process.wait_impl(Some(2.0))` (POSIX) or the ConPTY close-event
/// chain that occasionally lets cmd.exe surface the `Terminate batch job
/// (Y/N)?` prompt (Windows). The daemon's background thread does the
/// real kill_tree from out of the user's hot path.
pub(super) fn interrupt_pty_process(process: &NativePtyProcess, verbose: bool) -> i32 {
    let pid = process.pid().ok().flatten();
    let handed_off = match pid {
        Some(pid) => match crate::daemon::default_state_dir() {
            Ok(state_dir) => {
                let ok = crate::daemon::try_handoff_kill_to_daemon(
                    &state_dir,
                    &[pid],
                    Some("ctrl_c_pty"),
                );
                crate::ctrl_c_track::record_handoff(
                    ok,
                    Some(if ok {
                        "ctrl_c_pty"
                    } else {
                        "daemon_unreachable"
                    }),
                );
                ok
            }
            Err(_) => {
                crate::ctrl_c_track::record_handoff(false, Some("no_state_dir"));
                false
            }
        },
        None => {
            crate::ctrl_c_track::record_handoff(false, Some("no_child_pid"));
            false
        }
    };

    #[cfg(windows)]
    {
        if handed_off {
            // Daemon owns the kill. Skip ConPTY close so cmd.exe doesn't
            // get a chance to print "Terminate batch job (Y/N)?" before
            // the Job Object reaps it on our process exit.
            if verbose {
                verbose_log::log("[clud] interrupted via Ctrl+C (pty, handed to daemon)");
            }
            return 130;
        }
        // Closing the PTY triggers ConPTY's CTRL_CLOSE_EVENT path and
        // tears the child down without writing a second 0x03 byte.
        let _ = process.close_impl();
        if verbose {
            verbose_log::log("[clud] interrupted via Ctrl+C (pty)");
        }
        130
    }
    #[cfg(not(windows))]
    {
        if handed_off {
            // Daemon will SIGKILL the tree; skip the 2s blocking wait.
            let _ = process.close_impl();
            if verbose {
                verbose_log::log("[clud] interrupted via Ctrl+C (pty, handed to daemon)");
            }
            return 130;
        }
        // Belt-and-braces: portable-pty's `send_interrupt` queries
        // `tcgetpgrp(master_fd)` to find the FG pgroup and signals it.
        // If that query returns None (no controlling-terminal coupling
        // ever established, or the slave already lost FG), the library
        // falls back to writing a raw 0x03 byte to the master — which
        // only fires SIGINT if the slave still has ISIG set and the
        // child is actively reading. Tests that drive a sleep-only
        // child (issue #159) fail under that fallback because nothing
        // converts 0x03 to a signal. Send through both paths and also
        // signal the child's PID tree directly so all three vectors are
        // covered. Then `close_impl` ensures the master is torn down so
        // the pump loop doesn't keep polling a half-dead PTY.
        let _ = process.send_interrupt_impl();
        let tree_signal_result = process.terminate_tree_impl();
        let _ = process.wait_impl(Some(2.0));
        let _ = process.close_impl();
        if verbose {
            verbose_log::log("[clud] interrupted via Ctrl+C (pty)");
            if let Err(err) = tree_signal_result {
                verbose_log::log(format_args!(
                    "[clud] pty interrupt: tree-signal fallback failed: {err}"
                ));
            }
        }
        // Match the Windows branch above: always report SIGINT's
        // shell-convention 130 when clud itself handled the Ctrl-C.
        // The child's actual exit code is observable via the wait_impl
        // call above (stored in returncode for diagnostics); we just
        // don't propagate it, because the contract is "user pressed
        // Ctrl-C → exit 130" regardless of how the child shut down.
        130
    }
}
