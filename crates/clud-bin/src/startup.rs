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

pub fn install_ctrl_c_flag() -> Arc<AtomicBool> {
    use std::sync::atomic::Ordering;
    let interrupted = Arc::new(AtomicBool::new(false));
    let handler_flag = Arc::clone(&interrupted);
    if let Err(e) = ctrlc::set_handler(move || {
        handler_flag.store(true, Ordering::SeqCst);
    }) {
        eprintln!("[clud] warning: failed to install Ctrl+C handler: {}", e);
    }
    interrupted
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
}
