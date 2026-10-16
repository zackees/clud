//! Windows-only console drag-drop target adapter.
//!
//! ## Why this exists
//!
//! Issue #65: dragging a file onto the console window running `clud` on
//! Windows produces the OS "no-drop" cursor — the drop is rejected at
//! the OLE layer (`IDropTarget::DragEnter` → `DROPEFFECT_NONE`) before
//! any bytes reach `clud`'s stdin. The fix path described in #66 is to
//! have `clud` register **its own** `IDropTarget` on the window that
//! actually receives the Explorer drag. Under legacy conhost that is
//! usually `GetConsoleWindow()`. Under Windows Terminal, `GetConsoleWindow()`
//! is a `PseudoConsoleWindow`; Explorer hovers over the visible
//! `WindowsTerminal.exe` top-level window instead, so we register both
//! when possible. VS Code and WezTerm accept drops on their terminal
//! themselves, so they keep the console window only; the rule lives in
//! `dnd::drop_host` (#1358).
//!
//! Issue #79 adds a wrinkle: Claude Code (the backend) registers its
//! own `IDropTarget` after launch and overrides ours. Solution:
//!
//! - **Initial delay** before the first `RegisterDragDrop` call so
//!   Claude finishes its setup first (default: 2s, configurable via
//!   [`RefreshConfig::initial_delay`]).
//! - **Periodic refresh** — re-call `RegisterDragDrop` every N seconds
//!   so any subsequent Claude re-registration is displaced quickly
//!   (default: 3s, configurable via [`RefreshConfig::refresh_interval`]).
//!
//! ## What's here
//!
//! - The public registration function [`register_console_drop_target`]
//!   and a RAII guard [`ConsoleDropTargetGuard`] that revokes the
//!   registration, uninitializes OLE, and joins the refresh-loop worker
//!   thread when dropped.
//! - On Windows, the full COM wiring: `OleInitialize` → construct an
//!   `IDropTarget` whose `Drop()` callback hands `CF_HDROP` bytes to
//!   [`dispatch_dropfiles_to_injector`] → spawn a refresh worker that
//!   runs the delay-then-periodic-register strategy.
//! - The platform-agnostic dispatch half — given a raw `CF_HDROP` byte
//!   buffer, decode it via [`super::dropfiles::parse_dropfiles_buffer`],
//!   normalize each path via [`super::normalize_dropped_path`], and
//!   forward the result to the caller-supplied injector. Fully unit-
//!   tested on every CI host.
//! - [`DragDropRegistrar`] trait so the worker-loop logic is unit-
//!   testable without OLE.
//!
//! ## Threading contract (Windows)
//!
//! The refresh worker thread is the one that calls `OleInitialize` and
//! `RegisterDragDrop`. It must be a Single-Threaded-Apartment (STA)
//! thread for OLE drag-drop to function. The IDropTarget callbacks
//! themselves fire on whichever thread owns the console window's
//! message pump; COM marshals the call across apartments as needed.
//!
//! The guard's `Drop` impl signals the worker and joins it. The worker
//! owns the OLE apartment; before it exits, it revokes each registered
//! window and calls `OleUninitialize` on that same thread.
//!
//! ## What's still TODO (sub-agent B)
//!
//! - Wire `register_console_drop_target` into `main.rs`: that needs to
//!   know the launch mode (PTY vs subprocess) and pick a sensible
//!   injector. The COM machinery here is already complete.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::dnd::dropfiles::parse_dropfiles_buffer;
use crate::dnd::normalize_dropped_path;

/// Caller-supplied callback invoked once per drop, with the list of
/// parsed-and-normalized file paths from a `CF_HDROP` payload.
///
/// Implementations should be cheap and non-blocking — the callback runs
/// inline on the OLE drop notification, which on Windows is delivered
/// on the thread that owns the console window.
pub type DropInjector = Box<dyn Fn(&[String]) + Send + Sync + 'static>;

/// Failure modes for [`register_console_drop_target`].
#[derive(Debug)]
pub enum RegisterError {
    /// The Windows OLE wiring is compiled out (e.g. a build whose
    /// `[cfg(windows)]` blocks have been disabled for testing).
    /// Production builds on Windows do not return this variant —
    /// registration either succeeds or returns a more specific error
    /// below.
    NotImplemented,
    /// `register_console_drop_target` was called from a non-Windows
    /// build. The whole subsystem is Windows-only by design — POSIX
    /// terminals already deliver drops as stdin bytes that the #63
    /// normalizer handles.
    UnsupportedPlatform,
    /// `GetConsoleWindow()` returned `NULL` — `clud` is not attached
    /// to a console (e.g. running detached or under a service host).
    /// No drop target is possible.
    ConsoleWindowUnavailable,
    /// `OleInitialize` failed with the given `HRESULT`.
    OleInitializeFailed(i32),
    /// `RegisterDragDrop` failed with the given `HRESULT`. Common
    /// cause: a different process already owns the drop target, or
    /// UIPI is blocking the call.
    ///
    /// Refresh-loop failures *after* a successful initial registration
    /// are not surfaced through this variant — they are silently
    /// retried on the next tick (the whole point of the refresh loop).
    RegisterDragDropFailed(i32),
    /// Spawning the refresh worker thread failed.
    WorkerSpawnFailed,
}

impl std::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegisterError::NotImplemented => {
                f.write_str("console IDropTarget COM wiring is not compiled into this build")
            }
            RegisterError::UnsupportedPlatform => {
                f.write_str("console drop target is Windows-only")
            }
            RegisterError::ConsoleWindowUnavailable => {
                f.write_str("GetConsoleWindow() returned NULL — no console attached")
            }
            RegisterError::OleInitializeFailed(hr) => {
                write!(f, "OleInitialize failed (HRESULT 0x{:08x})", *hr as u32)
            }
            RegisterError::RegisterDragDropFailed(hr) => {
                write!(f, "RegisterDragDrop failed (HRESULT 0x{:08x})", *hr as u32)
            }
            RegisterError::WorkerSpawnFailed => {
                f.write_str("failed to spawn drag-drop refresh worker thread")
            }
        }
    }
}

impl std::error::Error for RegisterError {}

/// Tunables for the delay-then-refresh registration strategy.
///
/// See module-level docs for the why; defaults are chosen to displace
/// Claude Code's own `IDropTarget` registration without being so
/// aggressive that we burn cycles every second.
#[derive(Clone, Copy, Debug)]
pub struct RefreshConfig {
    /// How long to wait after the guard is created before the first
    /// `RegisterDragDrop` call. Lets the backend finish its own
    /// `OleInitialize` / `RegisterDragDrop` first so we register
    /// *after* it and win.
    pub initial_delay: Duration,
    /// How often to re-register after the initial registration. A
    /// `Duration::ZERO` here disables the refresh loop entirely (used
    /// in tests and one-shot mode).
    pub refresh_interval: Duration,
}

impl RefreshConfig {
    /// Production default: 2-second initial delay, 3-second refresh
    /// interval. Tuned in #79 to displace Claude Code's IDropTarget.
    pub fn default_displacement() -> Self {
        Self {
            initial_delay: Duration::from_secs(2),
            refresh_interval: Duration::from_secs(3),
        }
    }

    /// For tests / aggressive cases: register immediately and don't
    /// refresh.
    pub fn immediate_no_refresh() -> Self {
        Self {
            initial_delay: Duration::ZERO,
            refresh_interval: Duration::ZERO,
        }
    }
}

impl Default for RefreshConfig {
    fn default() -> Self {
        Self::default_displacement()
    }
}

/// Abstraction over the actual OLE `RegisterDragDrop` / `RevokeDragDrop`
/// calls so the worker-loop logic is unit-testable without touching
/// COM.
///
/// Production code uses the Windows-only `OleRegistrar`; tests use a
/// `MockRegistrar` that just counts calls. Public so it stays live on
/// non-Windows targets, which never wire up an `IDropTarget`.
pub trait DragDropRegistrar: Send + Sync {
    /// Register `clud` as the drop target. Returns the raw HRESULT on
    /// failure (so the worker loop can surface it via
    /// [`RegisterError::RegisterDragDropFailed`]).
    fn register(&self) -> Result<(), i32>;
    /// Revoke our registration. Best-effort — errors are logged but
    /// can't be surfaced from `Drop`. Only invoked from the
    /// Windows-only `OleKeepAlive::drop`; non-Windows test builds
    /// never call this.
    fn revoke(&self) -> Result<(), i32>;
}

/// Shared shutdown signal for the refresh-loop worker thread.
pub struct RefreshShutdown {
    flag: AtomicBool,
}

impl Default for RefreshShutdown {
    fn default() -> Self {
        Self::new()
    }
}

impl RefreshShutdown {
    /// Used only when constructing a live registration on Windows or
    /// in unit tests; non-Windows builds never construct a
    /// `RefreshShutdown`.
    pub fn new() -> Self {
        Self {
            flag: AtomicBool::new(false),
        }
    }
    pub fn signal(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }
    /// Read by `responsive_sleep` and `run_registration_loop`, both of
    /// which only run on Windows or in tests.
    pub fn is_signaled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// Sleep `total` in slices of at most `chunk`, exiting early if
/// `shutdown` is signaled. Returns `true` if the sleep completed
/// without a shutdown signal, `false` if interrupted.
pub fn responsive_sleep(total: Duration, shutdown: &RefreshShutdown) -> bool {
    if shutdown.is_signaled() {
        return false;
    }
    if total.is_zero() {
        crate::platform::dnd::pump_ole_messages();
        return true;
    }
    // Wake at most every 100ms so guard drop doesn't have to wait the
    // full refresh interval.
    let chunk = Duration::from_millis(100);
    let mut remaining = total;
    while !remaining.is_zero() {
        if shutdown.is_signaled() {
            return false;
        }
        let step = if remaining < chunk { remaining } else { chunk };
        // Windows pumps OLE messages around a `MsgWaitForMultipleObjects`;
        // elsewhere this is a plain sleep.
        crate::platform::dnd::wait_for_messages(step);
        remaining = remaining.saturating_sub(step);
    }
    !shutdown.is_signaled()
}

/// The worker-loop core, generic over `DragDropRegistrar` so it can be
/// tested without OLE.
///
/// Returns `Ok(())` after the loop exits cleanly via the shutdown
/// signal, or `Err(hr)` if the *initial* registration failed (so the
/// caller can surface `RegisterDragDropFailed(hr)`).
pub fn run_registration_loop<R: DragDropRegistrar + ?Sized>(
    registrar: &R,
    config: RefreshConfig,
    shutdown: &RefreshShutdown,
) -> Result<(), i32> {
    // Phase 1: initial delay — let the backend finish its own setup
    // first so we register *after* it.
    if !responsive_sleep(config.initial_delay, shutdown) {
        return Ok(());
    }

    // Phase 2: initial registration. Failure here is fatal — surface
    // the HRESULT so the caller gets `RegisterDragDropFailed`.
    registrar.register()?;

    // Phase 3: optional refresh loop. ZERO disables it.
    if config.refresh_interval.is_zero() {
        return Ok(());
    }
    while !shutdown.is_signaled() {
        if !responsive_sleep(config.refresh_interval, shutdown) {
            break;
        }
        // Refresh failures are non-fatal — Claude or some other COM
        // server may have temporarily owned the slot. Try again on
        // the next tick.
        let _ = registrar.register();
    }
    Ok(())
}

// ─── Guard ────────────────────────────────────────────────────────────

/// RAII guard returned by a successful registration. Dropping it
/// signals the refresh worker to exit, joins the worker thread, and
/// (on Windows) calls `RevokeDragDrop` + `OleUninitialize` so the
/// console window is left in the same state it was found in.
///
/// The guard is intentionally opaque so future fields can be added
/// without a breaking API change.
pub struct ConsoleDropTargetGuard {
    /// Set to None after Drop runs once. Optional so we can
    /// destructure for the worker join.
    pub(crate) inner: Option<GuardInner>,
}

pub(crate) struct GuardInner {
    pub(crate) shutdown: Arc<RefreshShutdown>,
    pub(crate) worker: Option<JoinHandle<()>>,
    /// Held to keep the registrar (and on Windows, the OLE state)
    /// alive until *after* the worker has joined. Boxed so the type
    /// of the closure is erased and we don't infect the public API.
    /// Dropped after the worker join in `Drop`.
    #[allow(dead_code)]
    pub(crate) keep_alive: Option<Box<dyn KeepAliveTrait>>,
}

/// Sealed marker — implementors are kept alive for the lifetime of the
/// guard and dropped after the worker has joined.
pub(crate) trait KeepAliveTrait: Send + Sync {}

impl Drop for ConsoleDropTargetGuard {
    fn drop(&mut self) {
        if let Some(mut inner) = self.inner.take() {
            inner.shutdown.signal();
            if let Some(handle) = inner.worker.take() {
                // Best-effort join — if the thread panicked we still
                // want to clean up.
                let _ = handle.join();
            }
            // `keep_alive` is dropped here, after the worker has
            // joined, so we don't free the registrar out from under a
            // final refresh-loop call.
            drop(inner.keep_alive.take());
        }
    }
}

// ─── Public entry point ───────────────────────────────────────────────

/// Register `clud` as the `IDropTarget` for the console window so
/// dropped files are routed to `injector` after parsing and
/// normalization.
///
/// `config` controls the delay-then-refresh strategy used to displace
/// the backend's own `IDropTarget` registration; see [`RefreshConfig`].
/// Most callers should pass [`RefreshConfig::default_displacement`].
///
/// On non-Windows this is a no-op returning [`RegisterError::UnsupportedPlatform`]
/// so callers can build cross-platform without target-cfg gates.
pub fn register_console_drop_target(
    injector: DropInjector,
    config: RefreshConfig,
) -> Result<ConsoleDropTargetGuard, RegisterError> {
    crate::platform::dnd::register_console_drop_target(injector, config)
}

/// Platform-agnostic glue: decode a `CF_HDROP` byte buffer, normalize
/// each path via [`normalize_dropped_path`], and forward the list to
/// the injector.
///
/// Exposed as a separate function (rather than buried inside the COM
/// callback) so it can be unit-tested on every CI host. The Windows
/// `IDropTarget::Drop` implementation is a thin wrapper around this —
/// extract the `CF_HDROP` HGLOBAL bytes, then hand them here.
///
/// Returns `true` when the injector was called, `false` when the buffer
/// held no paths (empty or malformed), so `Drop` can report
/// `DROPEFFECT_NONE` for a drop that delivered nothing.
pub fn dispatch_dropfiles_to_injector(buf: &[u8], injector: &DropInjector) -> bool {
    let parsed = parse_dropfiles_buffer(buf);
    if parsed.is_empty() {
        return false;
    }
    let normalized: Vec<String> = parsed.iter().map(|p| normalize_dropped_path(p)).collect();
    injector(&normalized);
    true
}

/// `DROPEFFECT_NONE` (oleidl.h) as a plain bit mask, so [`drag_effect`]
/// is testable on every host. A Windows test pins it to the `windows`
/// crate constant.
pub const DROPEFFECT_NONE_BITS: u32 = 0;
/// `DROPEFFECT_COPY` (oleidl.h); see [`DROPEFFECT_NONE_BITS`].
pub const DROPEFFECT_COPY_BITS: u32 = 1;

/// The effect clud's `IDropTarget` reports back to OLE from
/// `DragEnter`, `DragOver` and `Drop`.
///
/// `source_allowed` is the `*pdwEffect` value OLE passes in: the effects
/// the drag source permits. `carries_files` is whether the payload
/// offers `CF_HDROP` (during the drag) or whether the paths were
/// delivered to the injector (on `Drop`).
///
/// clud only ever reads the dropped paths, so the one effect it can
/// claim is `DROPEFFECT_COPY`. It must never claim `MOVE`: Explorer
/// deletes the source file after a move. It reports `NONE` when the
/// payload has no files, so a text or image drag shows the no-drop
/// cursor rather than a copy cursor for a drop that would do nothing,
/// and when the source does not allow a copy, because a target must
/// return an effect the source permits.
pub fn drag_effect(source_allowed: u32, carries_files: bool) -> u32 {
    if carries_files && source_allowed & DROPEFFECT_COPY_BITS != 0 {
        DROPEFFECT_COPY_BITS
    } else {
        DROPEFFECT_NONE_BITS
    }
}

// The Windows `IDropTarget` COM wiring lives in
// `platform/windows/dnd/console_drop_target.rs`.

#[cfg(test)]
#[path = "console_drop_target_tests.rs"]
pub(crate) mod tests;
