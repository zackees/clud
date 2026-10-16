//! Windows half of `crate::startup`: console drag-drop registration and
//! the `SetConsoleCtrlHandler` event-kind probe.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::dnd;
use crate::startup::should_report_registration_failure;

/// Subprocess-mode IDropTarget registration; see
/// `crate::startup::try_register_console_drop_target_subprocess`.
pub fn try_register_console_drop_target_subprocess(
) -> Option<dnd::console_drop_target::ConsoleDropTargetGuard> {
    use dnd::console_drop_target::{register_console_drop_target, RefreshConfig};
    let injector = dnd::injectors::subprocess_console_injector();
    match register_console_drop_target(injector, RefreshConfig::default_displacement()) {
        Ok(guard) => Some(guard),
        Err(e) => {
            if should_report_registration_failure(&e) {
                eprintln!("[clud] note: console drag-drop unavailable: {}", e);
            }
            None
        }
    }
}

/// PTY-mode IDropTarget registration. The injector writes into a
/// channel; the pump drains it each iteration and forwards into the
/// PTY master.
pub fn try_register_console_drop_target_pty() -> (
    Option<dnd::console_drop_target::ConsoleDropTargetGuard>,
    Option<std::sync::mpsc::Receiver<Vec<u8>>>,
) {
    use dnd::console_drop_target::{register_console_drop_target, RefreshConfig};
    use std::sync::{mpsc, Arc, Mutex};

    let (tx, rx) = mpsc::channel::<Vec<u8>>();

    // Adapter `Write` impl: each `write_all` from the OLE callback
    // becomes a `Vec<u8>` chunk in the channel. Send failure means the
    // pump exited (receiver dropped) — silently drop the bytes.
    struct ChannelWriter(mpsc::Sender<Vec<u8>>);
    impl std::io::Write for ChannelWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let n = buf.len();
            let _ = self.0.send(buf.to_vec());
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let writer: Box<dyn std::io::Write + Send> = Box::new(ChannelWriter(tx));
    let master = Arc::new(Mutex::new(writer));
    let injector = dnd::injectors::pty_master_injector(master);

    match register_console_drop_target(injector, RefreshConfig::default_displacement()) {
        Ok(guard) => (Some(guard), Some(rx)),
        Err(e) => {
            if should_report_registration_failure(&e) {
                eprintln!("[clud] note: console drag-drop unavailable: {}", e);
            }
            (None, None)
        }
    }
}

/// Install a `SetConsoleCtrlHandler` probe whose only job is to record
/// **which** console-control event the OS delivered — `CTRL_C_EVENT`
/// vs `CTRL_BREAK_EVENT` vs `CTRL_CLOSE_EVENT` etc. — so the dashboard
/// can tell a real Ctrl+C keypress apart from a
/// `GenerateConsoleCtrlEvent` broadcast made by some descendant in the
/// process tree.
///
/// The probe is installed **after** the `ctrlc` crate's handler. Windows
/// dispatches console-control handlers in **reverse order of
/// registration** (most recently installed first), so our probe fires
/// first, stamps the kind into [`crate::ctrl_c_track::record_event_kind`],
/// and returns `FALSE` to fall through to the `ctrlc` handler, which
/// preserves existing behavior (stamp the observation timestamp + flip
/// the interrupted flag).
///
/// No-op on non-Windows builds — the kind isn't ambiguous there, and
/// the `signal-hook` integration in the `ctrlc` crate already knows
/// which signal it caught.
pub fn install_windows_ctrl_event_probe() {
    use windows::Win32::System::Console::SetConsoleCtrlHandler;
    use windows_core::BOOL;

    // The probe MUST be `unsafe extern "system" fn` and MUST be stateless
    // (no closure captures), because the Win32 ABI calls it on an OS-
    // managed thread with no Rust context. All communication with the
    // rest of the process goes through the atomics in `ctrl_c_track`.
    //
    // We always return `FALSE` so the next handler in the chain (the
    // `ctrlc` crate's handler, registered earlier) gets a chance to
    // run. Returning `TRUE` here would short-circuit the chain and
    // prevent `ctrlc`'s `record_observed` + interrupted-flag work.
    unsafe extern "system" fn probe(ctrl_type: u32) -> BOOL {
        crate::ctrl_c_track::record_event_kind(crate::ctrl_c_track::CtrlEventKind::from_raw(
            ctrl_type,
        ));
        // `BOOL(0)` == FALSE — keep walking the handler chain.
        BOOL(0)
    }

    // SAFETY: `SetConsoleCtrlHandler` is documented as safe to call from
    // any thread at any time. `probe` is a `'static` function pointer
    // with the correct ABI, and we pass `TRUE` to add (not remove).
    let res = unsafe { SetConsoleCtrlHandler(Some(probe), true) };
    if let Err(e) = res {
        eprintln!(
            "[clud] warning: failed to install Windows ctrl-event probe: {}",
            e
        );
    }
}

pub fn install_unix_termination_probe(_interrupted: &Arc<AtomicBool>) {
    // SIGTERM/SIGHUP/SIGQUIT don't exist on Windows; the console-control
    // probe above already covers the equivalent close/logoff/shutdown
    // events.
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

    /// Issue #79 B3: registration failure (any `RegisterError` variant)
    /// must NOT abort the launch path. The `try_register_console_drop_target_*`
    /// helpers swallow errors and return `None` so the launch proceeds.
    ///
    /// On Windows we exercise this against the real
    /// `register_console_drop_target` — in a unit-test process there is
    /// typically no console window, so the registration returns
    /// `ConsoleWindowUnavailable`, exactly the failure variant we want
    /// to confirm is non-fatal.
    #[test]
    fn main_registration_failure_does_not_abort_launch() {
        // Subprocess injector — does no work synchronously; the OLE
        // failure on the worker thread is what we care about. Whether
        // it succeeds or fails, the function must return Option<Guard>
        // (never panic, never abort the process).
        let _result = crate::startup::try_register_console_drop_target_subprocess();
    }

    /// Windows: the first press in a fresh window is a soft interrupt.
    /// The interrupted flag does NOT flip, the press classification is
    /// recorded as `FirstSoft`, and the user-facing warning fires.
    #[test]
    fn first_press_on_windows_is_soft_interrupt() {
        use std::cell::Cell;
        use std::sync::atomic::Ordering;
        let _guard = crate::ctrl_c_track::test_state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        reset_handler_state();
        let interrupted = AtomicBool::new(false);
        let warning: Cell<Option<String>> = Cell::new(None);

        let decision = crate::startup::run_ctrl_c_handler(
            false,
            true,
            &interrupted,
            |_| {},
            |msg| warning.set(Some(msg.to_string())),
        );

        assert!(
            matches!(decision, crate::startup::CtrlCDecision::FirstSoft { .. }),
            "first press on Windows must be classified FirstSoft"
        );
        assert!(
            !interrupted.load(Ordering::SeqCst),
            "first press on Windows MUST NOT flip interrupted"
        );
        let msg = warning.into_inner().expect("warning emitted");
        assert!(
            msg.contains("press again"),
            "warning must tell the user to press again, got: {msg}"
        );
        assert_eq!(
            crate::ctrl_c_track::observed_press_kind(),
            Some(crate::ctrl_c_track::CtrlPressKind::FirstSoft)
        );
    }

    /// Windows: a second press inside the rapid-succession window
    /// flips the interrupted flag and stamps `SecondExit`.
    #[test]
    fn second_press_within_window_exits_on_windows() {
        use std::sync::atomic::Ordering;
        let _guard = crate::ctrl_c_track::test_state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        reset_handler_state();
        let interrupted = AtomicBool::new(false);

        // First press — soft.
        let first = crate::startup::run_ctrl_c_handler(false, true, &interrupted, |_| {}, |_| {});
        assert!(matches!(
            first,
            crate::startup::CtrlCDecision::FirstSoft { .. }
        ));
        assert!(!interrupted.load(Ordering::SeqCst));

        // Second press immediately after — well inside the default
        // 1500ms window.
        let second = crate::startup::run_ctrl_c_handler(false, true, &interrupted, |_| {}, |_| {});
        assert!(
            matches!(second, crate::startup::CtrlCDecision::Exit),
            "second press inside window must Exit"
        );
        assert!(
            interrupted.load(Ordering::SeqCst),
            "second press inside window MUST flip interrupted"
        );
        assert_eq!(
            crate::ctrl_c_track::observed_press_kind(),
            Some(crate::ctrl_c_track::CtrlPressKind::SecondExit)
        );
    }

    /// Windows: two presses spaced beyond the window — the second
    /// press is treated as a fresh first press, NOT as Exit. Drives
    /// the env-var override down to a 50ms window so the test can
    /// sleep past it without slowing the suite.
    #[test]
    fn two_presses_outside_window_both_soft_on_windows() {
        use std::sync::atomic::Ordering;
        let _guard = crate::ctrl_c_track::test_state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        reset_handler_state();
        // Smallest accepted window. Sleeping 120ms reliably passes it
        // even on coarse-grained Windows timers.
        std::env::set_var(crate::ctrl_c_track::ENV_DOUBLE_TAP_WINDOW_MS, "50");
        let interrupted = AtomicBool::new(false);

        let first = crate::startup::run_ctrl_c_handler(false, true, &interrupted, |_| {}, |_| {});
        assert!(matches!(
            first,
            crate::startup::CtrlCDecision::FirstSoft { .. }
        ));

        std::thread::sleep(std::time::Duration::from_millis(120));

        let second = crate::startup::run_ctrl_c_handler(false, true, &interrupted, |_| {}, |_| {});
        assert!(
            matches!(second, crate::startup::CtrlCDecision::FirstSoft { .. }),
            "press past the window must reset to FirstSoft, got {second:?}"
        );
        assert!(
            !interrupted.load(Ordering::SeqCst),
            "no Exit means interrupted MUST remain false"
        );
        std::env::remove_var(crate::ctrl_c_track::ENV_DOUBLE_TAP_WINDOW_MS);
    }

    /// Windows: the opt-out env var (`CLUD_NO_DOUBLE_CTRL_C=1`)
    /// disables the guard entirely, so the very first press exits
    /// like the pre-#377 behavior.
    #[test]
    fn opt_out_env_var_makes_first_press_exit_on_windows() {
        use std::sync::atomic::Ordering;
        let _guard = crate::ctrl_c_track::test_state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        reset_handler_state();
        std::env::set_var(crate::ctrl_c_track::ENV_DISABLE_DOUBLE_TAP, "1");

        let interrupted = AtomicBool::new(false);
        let decision =
            crate::startup::run_ctrl_c_handler(false, true, &interrupted, |_| {}, |_| {});
        assert!(
            matches!(decision, crate::startup::CtrlCDecision::Exit),
            "with the guard disabled, the first press must Exit"
        );
        assert!(
            interrupted.load(Ordering::SeqCst),
            "with the guard disabled, the first press MUST flip interrupted"
        );
        std::env::remove_var(crate::ctrl_c_track::ENV_DISABLE_DOUBLE_TAP);
    }
}
