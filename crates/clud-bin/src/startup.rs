//! Launch-time helpers: console drag-drop registration, session-cap
//! enforcement, and the Ctrl+C flag installer. Factored out of `main.rs`
//! so the entry point reads as orchestration rather than plumbing.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::args::Args;
use crate::dnd;
use crate::session_registry;

/// Issue #79: a launch should register the console IDropTarget unless
/// the user opted out (`--no-dnd`) or the run can't possibly need a
/// drop target (`--dry-run` returns before this is consulted, but the
/// helper still rejects it for symmetry / explicit testability).
///
/// Factored out so `main.rs`'s logic can be unit-tested without
/// touching OLE or spawning processes.
pub fn should_register_drop_target(args: &Args) -> bool {
    if args.no_dnd {
        return false;
    }
    if args.dry_run {
        return false;
    }
    cfg!(windows)
}

/// Subprocess-mode IDropTarget registration. Returns `None` on any
/// failure (logging a one-line warning to stderr), so a registration
/// hiccup never aborts the launch path.
pub fn try_register_console_drop_target_subprocess(
) -> Option<dnd::console_drop_target::ConsoleDropTargetGuard> {
    #[cfg(not(windows))]
    {
        None
    }
    #[cfg(windows)]
    {
        use dnd::console_drop_target::{register_console_drop_target, RefreshConfig};
        let injector = dnd::injectors::subprocess_console_injector();
        match register_console_drop_target(injector, RefreshConfig::default_displacement()) {
            Ok(guard) => Some(guard),
            Err(e) => {
                eprintln!("[clud] note: console drag-drop unavailable: {}", e);
                None
            }
        }
    }
}

/// PTY-mode IDropTarget registration. The injector writes into a
/// channel; the pump drains it each iteration and forwards into the
/// PTY master.
#[cfg(windows)]
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
            eprintln!("[clud] note: console drag-drop unavailable: {}", e);
            (None, None)
        }
    }
}

/// RAII guard for the session-registry row. On Drop, briefly re-acquires
/// the cross-process lock, opens the redb file, removes our row, and
/// closes. Best-effort — failures are silent because the next startup's
/// GC pass cleans up stale rows anyway (issue #73).
pub struct ScopedSessionGuard;

impl Drop for ScopedSessionGuard {
    fn drop(&mut self) {
        let _ = session_registry::run_shutdown_under_lock();
    }
}

/// Issue #73 / #138: enforce the live-session cap. Acquires a cross-process
/// advisory lock, opens redb, runs gc + cap-check + register-self, then
/// closes redb and releases the lock — all in one short critical section
/// at startup. On `Refuse` this calls `std::process::exit(1)` directly.
/// On `Warn` we print to stderr and continue. Failures to open / GC the
/// DB are *non-fatal*: we log to stderr and skip the cap check, because
/// breaking `clud` startup over a registry hiccup would be much worse
/// than the rare case where the guardrail is temporarily missing.
///
/// Returns a guard whose Drop removes our row on graceful exit. The guard
/// holds no DB or lock between startup and shutdown — concurrent `clud`
/// launches can therefore both run through this function without racing
/// on the redb file lock (which previously caused issue #138's
/// `Database already open` warning).
pub fn enforce_session_cap() -> Option<ScopedSessionGuard> {
    let cfg = session_registry::SessionRegistry::cap_config_from_env();
    let info = session_registry::SessionInfo::for_self(None, None);
    let outcome = match session_registry::run_startup_under_lock(&cfg, info) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("[clud] warning: could not open session registry: {e}");
            return None;
        }
    };
    match outcome.decision {
        session_registry::CapDecision::Allow => {}
        session_registry::CapDecision::Warn(count) => {
            eprintln!(
                "[clud] warning: {count} live clud sessions detected (warn threshold {warn}, cap {cap}). \
                 Set {env_max}=0 to disable, or wind down old sessions.",
                warn = cfg.warn,
                cap = cfg.max,
                env_max = session_registry::ENV_MAX_INSTANCES,
            );
        }
        session_registry::CapDecision::Refuse(count) => {
            eprintln!(
                "[clud] error: {count} live clud sessions exceed the cap of {cap}. \
                 Refusing to launch (fork-bomb guardrail, issue #73). \
                 Wind down old sessions, or override via {env_max}=<larger> / \
                 {env_max}=0 to disable.",
                cap = cfg.max,
                env_max = session_registry::ENV_MAX_INSTANCES,
            );
            std::process::exit(1);
        }
    }
    if outcome.registered {
        Some(ScopedSessionGuard)
    } else {
        None
    }
}

pub fn install_ctrl_c_flag(verbose: bool) -> Arc<AtomicBool> {
    let interrupted = Arc::new(AtomicBool::new(false));
    let handler_flag = Arc::clone(&interrupted);
    // Snapshot interactivity once at install time so the handler
    // doesn't have to call `isatty()` from the ctrlc thread on every
    // press. Issue #377's double-Ctrl+C guard is meant to protect
    // interactive users from accidental teardown; scripts and CI
    // (which redirect stdin/stderr) should keep the historical
    // single-press exit so existing automation isn't broken.
    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin())
        && std::io::IsTerminal::is_terminal(&std::io::stderr());
    if let Err(e) = ctrlc::set_handler(move || {
        run_ctrl_c_handler(
            verbose,
            interactive,
            handler_flag.as_ref(),
            |msg| crate::verbose_log::log(msg),
            |msg| {
                // Stderr so the "press again to exit" notice shows up
                // even when verbose-mode logging is off — the user
                // needs to see it regardless of logging configuration.
                eprintln!("{msg}");
            },
        );
    }) {
        eprintln!("[clud] warning: failed to install Ctrl+C handler: {}", e);
    }
    install_windows_ctrl_event_probe();
    install_unix_termination_probe(&interrupted);
    interrupted
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
#[cfg(unix)]
fn install_unix_termination_probe(interrupted: &Arc<AtomicBool>) {
    use signal_hook::consts::signal::{SIGHUP, SIGQUIT, SIGTERM};
    use signal_hook::iterator::Signals;

    let mut signals = match Signals::new([SIGTERM, SIGHUP, SIGQUIT]) {
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
        for sig in signals.forever() {
            crate::ctrl_c_track::record_observed();
            crate::ctrl_c_track::record_event_kind(unix_termination_signal_kind(sig));
            flag.store(true, Ordering::SeqCst);
        }
    });
}

/// Pure `signal number -> CtrlEventKind` mapping for the three signals
/// [`install_unix_termination_probe`] registers. Factored out so the
/// mapping is unit-testable without spawning a thread or sending a real
/// signal (the integration test in `tests/ctrlc_signal_kinds.rs` covers
/// the end-to-end real-signal path).
#[cfg(unix)]
fn unix_termination_signal_kind(sig: i32) -> crate::ctrl_c_track::CtrlEventKind {
    use signal_hook::consts::signal::{SIGHUP, SIGQUIT, SIGTERM};
    match sig {
        SIGTERM => crate::ctrl_c_track::CtrlEventKind::Term,
        SIGHUP => crate::ctrl_c_track::CtrlEventKind::Hup,
        SIGQUIT => crate::ctrl_c_track::CtrlEventKind::Quit,
        _ => crate::ctrl_c_track::CtrlEventKind::Unknown,
    }
}

#[cfg(not(unix))]
fn install_unix_termination_probe(_interrupted: &Arc<AtomicBool>) {
    // SIGTERM/SIGHUP/SIGQUIT don't exist on Windows; the console-control
    // probe above already covers the equivalent close/logoff/shutdown
    // events.
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
#[cfg(windows)]
fn install_windows_ctrl_event_probe() {
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

#[cfg(not(windows))]
fn install_windows_ctrl_event_probe() {
    // The probe needs `SetConsoleCtrlHandler`, which only exists on
    // Windows. On Unix the `ctrlc` crate's `signal-hook` backend
    // already disambiguates SIGINT from SIGTERM, so leaving the
    // event-kind field as `None` is the correct, honest answer.
}

/// Outcome of a single press through [`run_ctrl_c_handler`]. The Windows
/// double-Ctrl+C guard (issue #377) swallows the first press in a
/// rapid-succession window so users don't tear down clud (and any
/// long-running backend session it owns) by accidental keystroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CtrlCDecision {
    /// First press inside a fresh rapid-succession window. clud did
    /// not flip the interrupted flag; a follow-up press is required to
    /// exit. The child still received its own copy of the
    /// console-control event from the OS so it can cancel cooperatively.
    FirstSoft { window_ms: u64 },
    /// Press flipped the interrupted flag. Either the second press
    /// landed inside the window, the guard was disabled by env var,
    /// or the platform doesn't engage the guard (non-Windows).
    Exit,
}

/// Pure side-effecting body of the Ctrl+C handler closure, extracted so
/// unit tests can assert the verbose-emit and double-tap decisions
/// without installing a real signal handler (which would conflict with
/// cargo's test runner).
///
/// The handler:
/// 1. Atomically swaps in the current timestamp and reads the prior one
///    (signal-safe — see [`crate::ctrl_c_track::record_observed_returning_prior`]).
/// 2. On Windows (and only when the double-tap guard is engaged), checks
///    whether the prior press lands inside the rapid-succession window.
///    If not, classifies this as `FirstSoft` and skips flipping the
///    interrupted flag — clud stays alive so the user can decide whether
///    to interrupt again.
/// 3. Otherwise (second press in window / non-Windows / opt-out), flips
///    the interrupted flag.
/// 4. Stamps the press classification + elapsed-since-prior into the
///    forensic event so the dashboard can distinguish swallowed presses
///    from teardown presses.
///
/// `emit_warning` carries the "press again to exit" notice. Tests pass
/// a `Cell`-backed closure to capture it; the production caller passes
/// a real `eprintln!` so the user sees it in their shell even when
/// `--verbose` is off.
///
/// `interactive` is the install-time snapshot of whether stdin **and**
/// stderr are TTYs. The double-tap guard is gated on this so scripts
/// and CI (which redirect at least one of those handles) keep the
/// historical single-press exit. Without this gate the guard breaks
/// every Python integration test that drives clud via subprocess and
/// sends a single Ctrl+C — the test would sit waiting for a second
/// press that no automation knows to send.
pub(crate) fn run_ctrl_c_handler(
    verbose: bool,
    interactive: bool,
    interrupted: &AtomicBool,
    emit_verbose: impl FnOnce(&str),
    emit_warning: impl FnOnce(&str),
) -> CtrlCDecision {
    use crate::ctrl_c_track::{
        double_tap_enabled, double_tap_window_ms, record_elapsed_since_prior_ms,
        record_observed_returning_prior, record_press_kind, CtrlPressKind,
    };
    use std::sync::atomic::Ordering;

    // Issue #517: on Unix, `ctrlc` (without the `termination` feature)
    // only ever installs a SIGINT handler, so every press that reaches
    // this closure unambiguously means SIGINT/Ctrl+C. On Windows the
    // kind is already stamped by `install_windows_ctrl_event_probe`
    // *before* this handler runs (it can be CtrlC, CtrlBreak, ...), so
    // stamping CtrlC here unconditionally would silently overwrite a
    // more specific Windows event with the wrong one.
    #[cfg(not(windows))]
    crate::ctrl_c_track::record_event_kind(crate::ctrl_c_track::CtrlEventKind::CtrlC);

    let prior_ms = record_observed_returning_prior();
    let elapsed_ms = if prior_ms == 0 {
        None
    } else {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(prior_ms);
        Some(now_ms.saturating_sub(prior_ms))
    };
    if let Some(gap) = elapsed_ms {
        // record_elapsed_since_prior_ms uses 0 as "no prior" sentinel,
        // so coerce a genuine zero gap to 1ms to keep the forensic
        // field meaningful.
        record_elapsed_since_prior_ms(gap.max(1));
    }

    let window_ms = double_tap_window_ms();
    let guard_engaged = interactive && double_tap_enabled();
    let inside_window = matches!(elapsed_ms, Some(gap) if gap <= window_ms);

    let decision = if guard_engaged && !inside_window {
        CtrlCDecision::FirstSoft { window_ms }
    } else {
        CtrlCDecision::Exit
    };

    match decision {
        CtrlCDecision::FirstSoft { window_ms } => {
            record_press_kind(CtrlPressKind::FirstSoft);
            if verbose {
                emit_verbose("[clud] ctrl-c received (first press, soft interrupt)");
            }
            emit_warning(&format!(
                "[clud] Ctrl+C — press again within {window_ms}ms to exit"
            ));
        }
        CtrlCDecision::Exit => {
            record_press_kind(CtrlPressKind::SecondExit);
            if verbose {
                emit_verbose("[clud] ctrl-c received");
            }
            interrupted.store(true, Ordering::SeqCst);
        }
    }
    decision
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_from(argv: &[&str]) -> Args {
        let raw: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        Args::parse_from_raw(raw)
    }

    /// Issue #79 B3: a `--dry-run` invocation must NOT trigger any
    /// `RegisterDragDrop` side effect. We can't observe the OLE call
    /// directly from a unit test (it would require a console window),
    /// so we test the gating helper that `main()` consults.
    #[test]
    fn main_dry_run_does_not_register_drop_target() {
        let args = args_from(&["clud", "--dry-run", "-p", "hi"]);
        assert!(args.dry_run);
        assert!(
            !should_register_drop_target(&args),
            "--dry-run must short-circuit the drop-target registration"
        );
    }

    #[test]
    fn main_no_dnd_flag_disables_registration() {
        let args = args_from(&["clud", "--no-dnd"]);
        assert!(args.no_dnd);
        assert!(
            !should_register_drop_target(&args),
            "--no-dnd must opt out of drop-target registration"
        );
    }

    #[test]
    fn main_no_drag_drop_alias_disables_registration() {
        let args = args_from(&["clud", "--no-drag-drop"]);
        assert!(args.no_dnd);
        assert!(!should_register_drop_target(&args));
    }

    /// Default invocation: registration is requested on Windows, skipped
    /// on POSIX. The actual COM call is downstream of this gate.
    #[test]
    fn main_default_invocation_requests_registration_on_windows() {
        let args = args_from(&["clud", "-p", "hi"]);
        let want = cfg!(windows);
        assert_eq!(should_register_drop_target(&args), want);
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
    #[cfg(windows)]
    #[test]
    fn main_registration_failure_does_not_abort_launch() {
        // Subprocess injector — does no work synchronously; the OLE
        // failure on the worker thread is what we care about. Whether
        // it succeeds or fails, the function must return Option<Guard>
        // (never panic, never abort the process).
        let _result = super::try_register_console_drop_target_subprocess();
    }

    /// Reset the `ctrl_c_track` statics + relevant env vars so each
    /// double-tap test starts from a known state. Callers must already
    /// hold `crate::ctrl_c_track::test_state_lock()` to serialize.
    fn reset_handler_state() {
        crate::ctrl_c_track::reset_for_test();
        std::env::remove_var(crate::ctrl_c_track::ENV_DISABLE_DOUBLE_TAP);
        std::env::remove_var(crate::ctrl_c_track::ENV_DOUBLE_TAP_WINDOW_MS);
    }

    /// Verbose mode: every press that actually exits emits the
    /// `[clud] ctrl-c received` marker so the launch log shows the
    /// moment of interrupt, not just the eventual exit-code lines.
    /// Non-Windows: a single press exits, so the marker fires on the
    /// first press. Windows: the first press is a soft interrupt;
    /// we drive the second press inside the window to land on Exit.
    #[test]
    fn ctrl_c_handler_emits_verbose_marker_when_verbose() {
        use std::cell::Cell;
        use std::sync::atomic::Ordering;
        let _guard = crate::ctrl_c_track::test_state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        reset_handler_state();
        let interrupted = AtomicBool::new(false);
        let captured: Cell<Option<String>> = Cell::new(None);

        // First press: on Windows this is a soft interrupt and the
        // emit closure receives a different marker. To exercise the
        // Exit branch on every platform, prime the timestamp first
        // and then drive the press that lands inside the window.
        if cfg!(windows) {
            // Prime: this swallows the first press on Windows.
            let _ = super::run_ctrl_c_handler(false, true, &interrupted, |_| {}, |_| {});
        }

        let decision = super::run_ctrl_c_handler(
            true,
            true,
            &interrupted,
            |msg| captured.set(Some(msg.to_string())),
            |_| {},
        );

        assert!(
            matches!(decision, super::CtrlCDecision::Exit),
            "handler must Exit when the guard is not engaged or inside the window"
        );
        assert_eq!(
            captured.into_inner().as_deref(),
            Some("[clud] ctrl-c received"),
            "verbose handler must emit the ctrl-c marker on Exit"
        );
        assert!(
            interrupted.load(Ordering::SeqCst),
            "handler must flip the interrupted flag on Exit"
        );
    }

    /// Non-verbose mode: the verbose marker is suppressed. The
    /// interrupted flag still flips on Exit. Mirrors the test above
    /// but checks the suppressed-emit path.
    #[test]
    fn ctrl_c_handler_skips_verbose_marker_when_not_verbose() {
        use std::cell::Cell;
        use std::sync::atomic::Ordering;
        let _guard = crate::ctrl_c_track::test_state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        reset_handler_state();
        let interrupted = AtomicBool::new(false);

        if cfg!(windows) {
            // Prime: swallow the first soft press on Windows.
            let _ = super::run_ctrl_c_handler(false, true, &interrupted, |_| {}, |_| {});
        }

        let called = Cell::new(false);
        let decision = super::run_ctrl_c_handler(
            false,
            true,
            &interrupted,
            |_| {
                called.set(true);
            },
            |_| {},
        );
        assert!(
            matches!(decision, super::CtrlCDecision::Exit),
            "handler must Exit when the guard is not engaged or inside the window"
        );
        assert!(
            !called.into_inner(),
            "non-verbose handler must not call the emit closure on Exit"
        );
        assert!(
            interrupted.load(Ordering::SeqCst),
            "handler must still flip the interrupted flag without verbose"
        );
    }

    // ---------------------------------------------------------------
    // Issue #377: double-Ctrl+C guard handler behavior.
    // ---------------------------------------------------------------

    /// Windows: the first press in a fresh window is a soft interrupt.
    /// The interrupted flag does NOT flip, the press classification is
    /// recorded as `FirstSoft`, and the user-facing warning fires.
    #[cfg(windows)]
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

        let decision = super::run_ctrl_c_handler(
            false,
            true,
            &interrupted,
            |_| {},
            |msg| warning.set(Some(msg.to_string())),
        );

        assert!(
            matches!(decision, super::CtrlCDecision::FirstSoft { .. }),
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
    #[cfg(windows)]
    #[test]
    fn second_press_within_window_exits_on_windows() {
        use std::sync::atomic::Ordering;
        let _guard = crate::ctrl_c_track::test_state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        reset_handler_state();
        let interrupted = AtomicBool::new(false);

        // First press — soft.
        let first = super::run_ctrl_c_handler(false, true, &interrupted, |_| {}, |_| {});
        assert!(matches!(first, super::CtrlCDecision::FirstSoft { .. }));
        assert!(!interrupted.load(Ordering::SeqCst));

        // Second press immediately after — well inside the default
        // 1500ms window.
        let second = super::run_ctrl_c_handler(false, true, &interrupted, |_| {}, |_| {});
        assert!(
            matches!(second, super::CtrlCDecision::Exit),
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
    #[cfg(windows)]
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

        let first = super::run_ctrl_c_handler(false, true, &interrupted, |_| {}, |_| {});
        assert!(matches!(first, super::CtrlCDecision::FirstSoft { .. }));

        std::thread::sleep(std::time::Duration::from_millis(120));

        let second = super::run_ctrl_c_handler(false, true, &interrupted, |_| {}, |_| {});
        assert!(
            matches!(second, super::CtrlCDecision::FirstSoft { .. }),
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
    #[cfg(windows)]
    #[test]
    fn opt_out_env_var_makes_first_press_exit_on_windows() {
        use std::sync::atomic::Ordering;
        let _guard = crate::ctrl_c_track::test_state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        reset_handler_state();
        std::env::set_var(crate::ctrl_c_track::ENV_DISABLE_DOUBLE_TAP, "1");

        let interrupted = AtomicBool::new(false);
        let decision = super::run_ctrl_c_handler(false, true, &interrupted, |_| {}, |_| {});
        assert!(
            matches!(decision, super::CtrlCDecision::Exit),
            "with the guard disabled, the first press must Exit"
        );
        assert!(
            interrupted.load(Ordering::SeqCst),
            "with the guard disabled, the first press MUST flip interrupted"
        );
        std::env::remove_var(crate::ctrl_c_track::ENV_DISABLE_DOUBLE_TAP);
    }

    /// Non-Windows: the guard is not engaged. A single press exits,
    /// matching the documented "non-Windows behavior is intentionally
    /// unchanged" line in the acceptance criteria.
    #[cfg(not(windows))]
    #[test]
    fn first_press_on_non_windows_exits() {
        use std::sync::atomic::Ordering;
        let _guard = crate::ctrl_c_track::test_state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        reset_handler_state();
        let interrupted = AtomicBool::new(false);

        let decision = super::run_ctrl_c_handler(false, true, &interrupted, |_| {}, |_| {});
        assert!(
            matches!(decision, super::CtrlCDecision::Exit),
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
    #[cfg(not(windows))]
    #[test]
    fn ctrl_c_press_on_non_windows_records_ctrl_c_event_kind() {
        let _guard = crate::ctrl_c_track::test_state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        reset_handler_state();
        let interrupted = AtomicBool::new(false);

        assert_eq!(crate::ctrl_c_track::observed_event_kind(), None);
        let _ = super::run_ctrl_c_handler(false, true, &interrupted, |_| {}, |_| {});
        assert_eq!(
            crate::ctrl_c_track::observed_event_kind(),
            Some(crate::ctrl_c_track::CtrlEventKind::CtrlC),
            "SIGINT press must stamp CtrlEventKind::CtrlC on Unix"
        );
    }

    /// Issue #517: the SIGTERM/SIGHUP/SIGQUIT -> CtrlEventKind mapping
    /// used by the Unix termination probe.
    #[cfg(unix)]
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
    #[cfg(unix)]
    #[test]
    fn unix_termination_signal_kind_maps_unknown_signal_to_unknown() {
        assert_eq!(
            super::unix_termination_signal_kind(9999),
            crate::ctrl_c_track::CtrlEventKind::Unknown
        );
    }

    /// Non-interactive (scripts, CI, piped stdin) MUST keep the
    /// single-press exit semantics on every platform — Python
    /// integration tests in this repo drive clud via subprocess and
    /// send a single Ctrl+C; they would deadlock under the double-tap
    /// guard. Issue #377 is about protecting **interactive users**, not
    /// about changing the behavior contract automation depends on.
    #[test]
    fn non_interactive_keeps_single_press_exit_on_every_platform() {
        use std::sync::atomic::Ordering;
        let _guard = crate::ctrl_c_track::test_state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        reset_handler_state();
        let interrupted = AtomicBool::new(false);

        // interactive=false simulates piped stdin / non-TTY stderr
        // (the install-time snapshot would have come back false).
        let decision = super::run_ctrl_c_handler(false, false, &interrupted, |_| {}, |_| {});
        assert!(
            matches!(decision, super::CtrlCDecision::Exit),
            "non-interactive: the first press MUST Exit on every platform"
        );
        assert!(
            interrupted.load(Ordering::SeqCst),
            "non-interactive: the first press MUST flip interrupted"
        );
    }
}
