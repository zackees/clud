//! Unix half of `crate::startup`: there is no console IDropTarget or
//! console-control probe here; SIGTERM/SIGHUP/SIGQUIT are observed
//! through `signal_hook` instead.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::dnd;

/// Always `None`: the console IDropTarget exists on Windows only.
pub fn try_register_console_drop_target_subprocess(
) -> Option<dnd::console_drop_target::ConsoleDropTargetGuard> {
    None
}

/// PTY-mode IDropTarget registration: always `(None, None)` here, the
/// console IDropTarget exists on Windows only.
pub fn try_register_console_drop_target_pty() -> (
    Option<dnd::console_drop_target::ConsoleDropTargetGuard>,
    Option<std::sync::mpsc::Receiver<Vec<u8>>>,
) {
    (None, None)
}

/// Issue #517: install clud's own handler for `SIGTERM` / `SIGHUP` /
/// `SIGQUIT` — three signals the unmodified `ctrlc` crate never touches
/// (its `termination` feature, which would add `SIGTERM`/`SIGHUP`, is not
/// enabled). Without this, `SIGHUP` (controlling terminal closed) and
/// `SIGQUIT` (Ctrl+\\) kill the process under the OS default disposition
/// before any clud code runs, and `SIGTERM` (e.g. `docker stop`) is
/// likewise unobserved.
///
/// Deliberately mirrors [`install_windows_ctrl_event_probe`]'s "record the
/// specific kind, then flip the same flag `ctrlc` flips for Ctrl+C" shape
/// — but the mechanism differs because Unix has no OS-level handler
/// chaining the way `SetConsoleCtrlHandler` does. Rather than replacing
/// `ctrlc`'s installed `SIGINT` handler (which a second raw `sigaction`
/// call would silently clobber), this claims three signals `ctrlc` never
/// registers at all, so there is nothing to chain to and no vendoring of
/// `ctrlc` is required.
///
/// Uses `signal_hook::iterator::Signals` — the same pattern already used
/// for `SIGWINCH` in `session.rs` — which delivers signals to a plain
/// background thread via a self-pipe rather than running our code inside
/// actual signal-handler context, so no `unsafe`/async-signal-safety
/// reasoning is needed here (unlike the Windows probe above, which must
/// run inside the OS console-handler thread).
///
/// Behavior change to be aware of: once this is installed, clud no
/// longer *dies* on `SIGTERM`/`SIGHUP`/`SIGQUIT` — it sets `interrupted`,
/// same as Ctrl+C, which is only honored by code paths that poll it. A
/// path that never checks `interrupted` will now ignore these signals
/// entirely rather than being killed by them, until whatever sent the
/// signal escalates (e.g. `docker stop`'s SIGKILL after its grace
/// window). This is the intended trade — "observe and decide" instead of
/// "die immediately" — but is worth knowing before assuming a hung
/// process will always respond to a bare SIGTERM.
pub fn install_unix_termination_probe(interrupted: &Arc<AtomicBool>) {
    use signal_hook::consts::signal::{SIGHUP, SIGQUIT, SIGTERM};
    use signal_hook::iterator::exfiltrator::WithRawSiginfo;
    use signal_hook::iterator::SignalsInfo;

    // #520: `WithRawSiginfo` copies `siginfo_t` out of the handler into the
    // self-pipe, so its sender is read on this ordinary thread with no
    // async-signal-safety reasoning of our own.
    let mut signals = match SignalsInfo::<WithRawSiginfo>::new([SIGTERM, SIGHUP, SIGQUIT]) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "[clud] warning: failed to install SIGTERM/SIGHUP/SIGQUIT handler: {}",
                e
            );
            return;
        }
    };
    let flag = Arc::clone(interrupted);
    std::thread::spawn(move || {
        use std::sync::atomic::Ordering;
        for info in signals.forever() {
            crate::ctrl_c_track::record_observed();
            crate::ctrl_c_track::record_signal_origin(signal_origin(&info));
            crate::ctrl_c_track::record_event_kind(unix_termination_signal_kind(info.si_signo));
            flag.store(true, Ordering::SeqCst);
        }
    });
}

/// The sender `siginfo_t` names (#520). Only a signal sent by a process
/// (`kill(2)`, `SI_USER`) or `sigqueue(3)` (`SI_QUEUE`) carries one; a
/// terminal-generated SIGHUP or SIGQUIT does not.
fn signal_origin(info: &libc::siginfo_t) -> Option<crate::ctrl_c_track::SignalOrigin> {
    if info.si_code != libc::SI_USER && info.si_code != libc::SI_QUEUE {
        return None;
    }
    // SAFETY: for SI_USER/SI_QUEUE the kernel fills the `_kill`/`_rt`
    // member, whose leading pid/uid fields these accessors read.
    let (pid, uid) = unsafe { (info.si_pid(), info.si_uid()) };
    Some(crate::ctrl_c_track::SignalOrigin {
        pid: u32::try_from(pid).unwrap_or(0),
        uid,
    })
}

/// Pure `signal number -> CtrlEventKind` mapping for the three signals
/// [`install_unix_termination_probe`] registers. Factored out so the
/// mapping is unit-testable without spawning a thread or sending a real
/// signal (the integration test in `tests/integration/signals/platform/unix/ctrlc_signal_kinds.rs` covers
/// the end-to-end real-signal path).
pub(crate) fn unix_termination_signal_kind(sig: i32) -> crate::ctrl_c_track::CtrlEventKind {
    use signal_hook::consts::signal::{SIGHUP, SIGQUIT, SIGTERM};
    match sig {
        SIGTERM => crate::ctrl_c_track::CtrlEventKind::Term,
        SIGHUP => crate::ctrl_c_track::CtrlEventKind::Hup,
        SIGQUIT => crate::ctrl_c_track::CtrlEventKind::Quit,
        _ => crate::ctrl_c_track::CtrlEventKind::Unknown,
    }
}

pub fn install_windows_ctrl_event_probe() {
    // The probe needs `SetConsoleCtrlHandler`, which only exists on
    // Windows. On Unix the `ctrlc` crate's `signal-hook` backend
    // already disambiguates SIGINT from SIGTERM, so leaving the
    // event-kind field as `None` is the correct, honest answer.
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    /// Reset the `ctrl_c_track` statics + relevant env vars so each
    /// double-tap test starts from a known state. Callers must already
    /// hold `crate::ctrl_c_track::test_state_lock()` to serialize.
    fn reset_handler_state() {
        crate::ctrl_c_track::reset_for_test();
        std::env::remove_var(crate::ctrl_c_track::ENV_DISABLE_DOUBLE_TAP);
        std::env::remove_var(crate::ctrl_c_track::ENV_DOUBLE_TAP_WINDOW_MS);
    }

    /// Non-Windows: the guard is not engaged. A single press exits,
    /// matching the documented "non-Windows behavior is intentionally
    /// unchanged" line in the acceptance criteria.
    #[test]
    fn first_press_on_non_windows_exits() {
        use std::sync::atomic::Ordering;
        let _guard = crate::ctrl_c_track::test_state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        reset_handler_state();
        let interrupted = AtomicBool::new(false);

        let decision =
            crate::startup::run_ctrl_c_handler(false, true, &interrupted, |_| {}, |_| {});
        assert!(
            matches!(decision, crate::startup::CtrlCDecision::Exit),
            "non-Windows: a single press must Exit"
        );
        assert!(
            interrupted.load(Ordering::SeqCst),
            "non-Windows: a single press MUST flip interrupted"
        );
    }

    /// Issue #517: on Unix, every press through `run_ctrl_c_handler` is
    /// unambiguously SIGINT (ctrlc's `termination` feature is off, so
    /// nothing else routes through this closure) and must stamp
    /// `CtrlEventKind::CtrlC` so the dashboard's `ctrl_event_kind` field
    /// is populated on Unix the same way the Windows probe populates it.
    #[test]
    fn ctrl_c_press_on_non_windows_records_ctrl_c_event_kind() {
        let _guard = crate::ctrl_c_track::test_state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        reset_handler_state();
        let interrupted = AtomicBool::new(false);

        assert_eq!(crate::ctrl_c_track::observed_event_kind(), None);
        let _ = crate::startup::run_ctrl_c_handler(false, true, &interrupted, |_| {}, |_| {});
        assert_eq!(
            crate::ctrl_c_track::observed_event_kind(),
            Some(crate::ctrl_c_track::CtrlEventKind::CtrlC),
            "SIGINT press must stamp CtrlEventKind::CtrlC on Unix"
        );
    }

    /// Issue #517: the SIGTERM/SIGHUP/SIGQUIT -> CtrlEventKind mapping
    /// used by the Unix termination probe.
    #[test]
    fn unix_termination_signal_kind_maps_known_signals() {
        use signal_hook::consts::signal::{SIGHUP, SIGQUIT, SIGTERM};
        assert_eq!(
            super::unix_termination_signal_kind(SIGTERM),
            crate::ctrl_c_track::CtrlEventKind::Term
        );
        assert_eq!(
            super::unix_termination_signal_kind(SIGHUP),
            crate::ctrl_c_track::CtrlEventKind::Hup
        );
        assert_eq!(
            super::unix_termination_signal_kind(SIGQUIT),
            crate::ctrl_c_track::CtrlEventKind::Quit
        );
    }

    /// Any signal number outside the registered set (which should never
    /// happen in practice, since `Signals::new` only ever delivers what
    /// it was constructed with) must funnel into `Unknown` rather than
    /// panicking — matches the same defensive default as
    /// `CtrlEventKind::from_raw`.
    #[test]
    fn unix_termination_signal_kind_maps_unknown_signal_to_unknown() {
        assert_eq!(
            super::unix_termination_signal_kind(9999),
            crate::ctrl_c_track::CtrlEventKind::Unknown
        );
    }
}
