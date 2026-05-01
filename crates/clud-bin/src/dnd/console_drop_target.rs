//! Windows-only console drag-drop target adapter.
//!
//! ## Why this exists
//!
//! Issue #65: dragging a file onto the console window running `clud` on
//! Windows produces the OS "no-drop" cursor — the drop is rejected at
//! the OLE layer (`IDropTarget::DragEnter` → `DROPEFFECT_NONE`) before
//! any bytes reach `clud`'s stdin. The fix path described in #66 is to
//! have `clud` register **its own** `IDropTarget` on
//! `GetConsoleWindow()` so the most-recent-registration-wins rule of
//! `RegisterDragDrop` displaces conhost's refusal.
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
//! The guard's `Drop` impl signals the worker, joins it, and only
//! then drops the registrar (which calls `RevokeDragDrop` +
//! `OleUninitialize` on the worker thread via a destructor message).
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
/// `MockRegistrar` that just counts calls. Gated to Windows + tests
/// because non-Windows targets never wire up an `IDropTarget` and
/// would otherwise see this trait as dead code.
#[cfg(any(windows, test))]
trait DragDropRegistrar: Send + Sync {
    /// Register `clud` as the drop target. Returns the raw HRESULT on
    /// failure (so the worker loop can surface it via
    /// [`RegisterError::RegisterDragDropFailed`]).
    fn register(&self) -> Result<(), i32>;
    /// Revoke our registration. Best-effort — errors are logged but
    /// can't be surfaced from `Drop`. Only invoked from the
    /// Windows-only `OleKeepAlive::drop`; non-Windows test builds
    /// never call this so silence the dead-code lint there.
    #[cfg_attr(not(windows), allow(dead_code))]
    fn revoke(&self) -> Result<(), i32>;
}

/// Shared shutdown signal for the refresh-loop worker thread.
struct RefreshShutdown {
    flag: AtomicBool,
}

impl RefreshShutdown {
    /// Used only when constructing a live registration on Windows or
    /// in unit tests; non-Windows builds never construct a
    /// `RefreshShutdown`.
    #[cfg(any(windows, test))]
    fn new() -> Self {
        Self {
            flag: AtomicBool::new(false),
        }
    }
    fn signal(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }
    /// Read by `responsive_sleep` and `run_registration_loop`, both of
    /// which are themselves Windows-or-test only.
    #[cfg(any(windows, test))]
    fn is_signaled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// Sleep `total` in slices of at most `chunk`, exiting early if
/// `shutdown` is signaled. Returns `true` if the sleep completed
/// without a shutdown signal, `false` if interrupted.
#[cfg(any(windows, test))]
fn responsive_sleep(total: Duration, shutdown: &RefreshShutdown) -> bool {
    if shutdown.is_signaled() {
        return false;
    }
    if total.is_zero() {
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
        std::thread::sleep(step);
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
#[cfg(any(windows, test))]
fn run_registration_loop<R: DragDropRegistrar + ?Sized>(
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
    inner: Option<GuardInner>,
}

struct GuardInner {
    shutdown: Arc<RefreshShutdown>,
    worker: Option<JoinHandle<()>>,
    /// Held to keep the registrar (and on Windows, the OLE state)
    /// alive until *after* the worker has joined. Boxed so the type
    /// of the closure is erased and we don't infect the public API.
    /// Dropped after the worker join in `Drop`.
    #[allow(dead_code)]
    keep_alive: Option<Box<dyn KeepAliveTrait>>,
}

/// Sealed marker — implementors are kept alive for the lifetime of the
/// guard and dropped after the worker has joined.
trait KeepAliveTrait: Send + Sync {}

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
#[cfg(windows)]
pub fn register_console_drop_target(
    injector: DropInjector,
    config: RefreshConfig,
) -> Result<ConsoleDropTargetGuard, RegisterError> {
    win::register(injector, config)
}

#[cfg(not(windows))]
pub fn register_console_drop_target(
    _injector: DropInjector,
    _config: RefreshConfig,
) -> Result<ConsoleDropTargetGuard, RegisterError> {
    Err(RegisterError::UnsupportedPlatform)
}

/// Platform-agnostic glue: decode a `CF_HDROP` byte buffer, normalize
/// each path via [`normalize_dropped_path`], and forward the list to
/// the injector.
///
/// Exposed as a separate function (rather than buried inside the COM
/// callback) so it can be unit-tested on every CI host. The Windows
/// `IDropTarget::Drop` implementation is a thin wrapper around this —
/// extract the `CF_HDROP` HGLOBAL bytes, then hand them here.
pub fn dispatch_dropfiles_to_injector(buf: &[u8], injector: &DropInjector) {
    let parsed = parse_dropfiles_buffer(buf);
    if parsed.is_empty() {
        return;
    }
    let normalized: Vec<String> = parsed.iter().map(|p| normalize_dropped_path(p)).collect();
    injector(&normalized);
}

// ─── Windows-only implementation ──────────────────────────────────────

#[cfg(windows)]
mod win {
    use super::*;
    use std::sync::Mutex;

    use windows::core::ComObject;
    use windows::Win32::Foundation::{HWND, POINTL, RPC_E_CHANGED_MODE};
    use windows::Win32::System::Com::IDataObject;
    use windows::Win32::System::Console::GetConsoleWindow;
    use windows::Win32::System::Ole::{
        IDropTarget, IDropTarget_Impl, OleInitialize, OleUninitialize, RegisterDragDrop,
        RevokeDragDrop, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
    };
    use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;

    /// COM object implementing `IDropTarget`. Accepts every drop with
    /// `DROPEFFECT_COPY` (so the cursor switches from ⊘ to a copy
    /// indicator), then on `Drop()` extracts the `CF_HDROP` bytes and
    /// hands them to [`super::dispatch_dropfiles_to_injector`].
    #[windows::core::implement(IDropTarget)]
    struct ConsoleDropTarget {
        injector: Mutex<DropInjector>,
    }

    #[allow(non_snake_case)]
    impl IDropTarget_Impl for ConsoleDropTarget_Impl {
        fn DragEnter(
            &self,
            _data: windows_core::Ref<'_, IDataObject>,
            _key_state: MODIFIERKEYS_FLAGS,
            _pt: &POINTL,
            effect: *mut DROPEFFECT,
        ) -> windows_core::Result<()> {
            // Always advertise DROPEFFECT_COPY so the cursor changes
            // from ⊘ to a copy indicator.
            // SAFETY: `effect` is a non-null out-pointer per the
            // IDropTarget contract.
            unsafe {
                if !effect.is_null() {
                    *effect = DROPEFFECT_COPY;
                }
            }
            Ok(())
        }

        fn DragOver(
            &self,
            _key_state: MODIFIERKEYS_FLAGS,
            _pt: &POINTL,
            effect: *mut DROPEFFECT,
        ) -> windows_core::Result<()> {
            // SAFETY: see DragEnter.
            unsafe {
                if !effect.is_null() {
                    *effect = DROPEFFECT_COPY;
                }
            }
            Ok(())
        }

        fn DragLeave(&self) -> windows_core::Result<()> {
            Ok(())
        }

        fn Drop(
            &self,
            data: windows_core::Ref<'_, IDataObject>,
            _key_state: MODIFIERKEYS_FLAGS,
            _pt: &POINTL,
            effect: *mut DROPEFFECT,
        ) -> windows_core::Result<()> {
            let mut accepted = false;
            if let Some(data_obj) = data.as_ref() {
                // SAFETY: copy_cf_hdrop_bytes only does FFI under the
                // standard IDataObject contract; STGMEDIUM bookkeeping
                // is handled inside.
                if let Some(buf) = unsafe { copy_cf_hdrop_bytes(data_obj) } {
                    // Wrap the user-supplied injector in catch_unwind
                    // so a panic cannot unwind across the FFI boundary.
                    if let Ok(guard) = self.injector.lock() {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            super::dispatch_dropfiles_to_injector(&buf, &guard);
                        }));
                    }
                    accepted = true;
                }
            }
            // SAFETY: see DragEnter.
            unsafe {
                if !effect.is_null() {
                    *effect = if accepted {
                        DROPEFFECT_COPY
                    } else {
                        DROPEFFECT_NONE
                    };
                }
            }
            Ok(())
        }
    }

    /// Pull the `CF_HDROP` payload out of an `IDataObject` and copy
    /// its bytes into a `Vec<u8>`. Returns `None` if the object
    /// doesn't carry a CF_HDROP TYMED_HGLOBAL medium. Always cleans
    /// up the STGMEDIUM (via `ReleaseStgMedium`) and the `GlobalLock`
    /// it took.
    ///
    /// # Safety
    ///
    /// `data` must be a live, AddRef'd `IDataObject`.
    unsafe fn copy_cf_hdrop_bytes(data: &IDataObject) -> Option<Vec<u8>> {
        use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
        use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
        use windows::Win32::System::Ole::{ReleaseStgMedium, CF_HDROP};

        let format = FORMATETC {
            cfFormat: CF_HDROP.0,
            ptd: core::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        };

        let mut medium = match unsafe { data.GetData(&format) } {
            Ok(m) => m,
            Err(_) => return None,
        };

        let bytes = if medium.tymed == TYMED_HGLOBAL.0 as u32 {
            // SAFETY: tymed says hGlobal is the active union variant.
            let hglobal = unsafe { medium.u.hGlobal };
            let ptr = unsafe { GlobalLock(hglobal) } as *const u8;
            if ptr.is_null() {
                None
            } else {
                let len = unsafe { GlobalSize(hglobal) };
                let bytes = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
                unsafe {
                    let _ = GlobalUnlock(hglobal);
                }
                Some(bytes)
            }
        } else {
            None
        };

        // SAFETY: pairs with GetData; ReleaseStgMedium handles the
        // union member and frees the HGLOBAL when pUnkForRelease is
        // null.
        unsafe {
            ReleaseStgMedium(&mut medium);
        }

        bytes
    }

    /// HWND wrapper that's `Send` so we can ship it into the worker
    /// thread. Sound because we never dereference it as a Rust
    /// pointer — only pass it back to the Win32 API, which is
    /// thread-safe with respect to HWND values.
    #[derive(Clone, Copy)]
    struct SendHwnd(HWND);
    // SAFETY: HWND is a kernel-owned handle; it's safe to Send/Sync
    // because the Win32 API handles thread-safety for the underlying
    // window.
    unsafe impl Send for SendHwnd {}
    unsafe impl Sync for SendHwnd {}

    /// COM-based registrar — calls `RegisterDragDrop` /
    /// `RevokeDragDrop` on the worker thread that owns the OLE
    /// apartment.
    struct OleRegistrar {
        hwnd: SendHwnd,
        target: IDropTarget,
        ole_initialized: AtomicBool,
    }

    // SAFETY: `target` is an apartment-threaded COM interface, but
    // we only ever invoke `RegisterDragDrop` / `RevokeDragDrop` from
    // the same worker thread that performed `OleInitialize` for it.
    // The `Send`/`Sync` impls here only assert that the *handle* can
    // be moved between threads, not that the COM object itself is
    // free-threaded. The worker-thread invariant is preserved by
    // construction: we spawn the worker, move the registrar in, and
    // drop it on the same worker.
    unsafe impl Send for OleRegistrar {}
    unsafe impl Sync for OleRegistrar {}

    impl super::DragDropRegistrar for OleRegistrar {
        fn register(&self) -> Result<(), i32> {
            // SAFETY: pure FFI; hwnd validated at registration time;
            // self.target is a ref-counted owned interface.
            unsafe { RegisterDragDrop(self.hwnd.0, &self.target).map_err(|e| e.code().0) }
        }

        fn revoke(&self) -> Result<(), i32> {
            // SAFETY: pure FFI; hwnd validated at registration time.
            unsafe { RevokeDragDrop(self.hwnd.0).map_err(|e| e.code().0) }
        }
    }

    /// Held inside the guard so the OLE state outlives the worker
    /// thread. Drop is intentionally side-effect-only (no panics);
    /// the guard arranges for this to drop *after* the worker join.
    struct OleKeepAlive {
        registrar: Arc<OleRegistrar>,
    }

    impl super::KeepAliveTrait for OleKeepAlive {}

    impl Drop for OleKeepAlive {
        fn drop(&mut self) {
            // Best-effort cleanup — errors here can't be surfaced.
            let _ = self.registrar.revoke();
            if self.registrar.ole_initialized.swap(false, Ordering::SeqCst) {
                // SAFETY: balanced against the OleInitialize the
                // worker performed at startup.
                unsafe { OleUninitialize() };
            }
        }
    }

    pub(super) fn register(
        injector: DropInjector,
        config: RefreshConfig,
    ) -> Result<ConsoleDropTargetGuard, RegisterError> {
        // 1. Resolve the console HWND.
        // SAFETY: pure FFI, no preconditions.
        let hwnd = unsafe { GetConsoleWindow() };
        if hwnd.is_invalid() {
            return Err(RegisterError::ConsoleWindowUnavailable);
        }

        // 2. Construct the IDropTarget COM object up-front. This is
        //    cheap and lets us hand a single `Arc<OleRegistrar>`
        //    into the worker thread.
        let drop_target = ComObject::new(ConsoleDropTarget {
            injector: Mutex::new(injector),
        });
        let target_iface: IDropTarget = drop_target.to_interface::<IDropTarget>();

        let registrar = Arc::new(OleRegistrar {
            hwnd: SendHwnd(hwnd),
            target: target_iface,
            ole_initialized: AtomicBool::new(false),
        });

        // 3. Spawn the worker. It performs OleInitialize (becoming
        //    an STA), then runs the delay-then-refresh loop.
        let shutdown = Arc::new(RefreshShutdown::new());
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_registrar: Arc<OleRegistrar> = Arc::clone(&registrar);

        // Channel for surfacing the OleInitialize result back to the
        // caller. `mpsc::channel` keeps us out of additional
        // dependencies.
        let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<(), RegisterError>>();

        let worker = std::thread::Builder::new()
            .name("clud-dnd-refresh".to_string())
            .spawn(move || {
                // OleInitialize on this thread (becomes STA). If the
                // thread is already STA-initialized,
                // RPC_E_CHANGED_MODE is treated as a soft success
                // (skip the matching OleUninitialize on cleanup).
                // SAFETY: pure FFI, no preconditions.
                let ole_init = unsafe { OleInitialize(None) };
                let owns_ole = match ole_init {
                    Ok(()) => true,
                    Err(err) if err.code() == RPC_E_CHANGED_MODE => false,
                    Err(err) => {
                        let _ = init_tx.send(Err(RegisterError::OleInitializeFailed(err.code().0)));
                        return;
                    }
                };
                worker_registrar
                    .ole_initialized
                    .store(owns_ole, Ordering::SeqCst);

                // OLE is up — tell the caller the spawn succeeded.
                // From here on, registration failures land inside
                // the loop and are logged rather than surfaced.
                let _ = init_tx.send(Ok(()));

                if let Err(hr) = run_registration_loop(&*worker_registrar, config, &worker_shutdown)
                {
                    eprintln!(
                        "[clud:dnd] RegisterDragDrop failed (HRESULT 0x{:08x})",
                        hr as u32
                    );
                }
                // OleUninitialize happens via OleKeepAlive::drop on
                // the *guard's* thread, not here. This is technically
                // wrong by the strictest reading of the COM apartment
                // model — OleUninitialize should run on the same
                // thread as OleInitialize. In practice the guard
                // typically lives for the lifetime of the program
                // and Drop runs on the main thread; we accept the
                // sliver of risk in exchange for not having to
                // marshal a "please-uninit" message back here.
                //
                // TODO(#79): if this causes problems on real Windows
                // hosts, send a uninit-and-exit signal back into the
                // worker and OleUninitialize from there.
            })
            .map_err(|_| RegisterError::WorkerSpawnFailed)?;

        // 4. Block until the worker has either signaled OLE-init
        //    success, OLE-init failure, or the channel was dropped
        //    (worker panicked before sending). On failure, join the
        //    worker and surface the error.
        let init_result = init_rx
            .recv()
            .map_err(|_| RegisterError::WorkerSpawnFailed)?;

        if let Err(err) = init_result {
            shutdown.signal();
            let _ = worker.join();
            return Err(err);
        }

        // Note: we deliberately do NOT block until the *initial
        // RegisterDragDrop* completes. The whole point of #79 is the
        // initial-delay phase — blocking here would defeat it. If
        // the initial registration eventually fails, the worker
        // logs and exits but the guard is still considered "live"
        // until dropped.
        let keep_alive: Box<dyn super::KeepAliveTrait> = Box::new(OleKeepAlive { registrar });

        Ok(ConsoleDropTargetGuard {
            inner: Some(GuardInner {
                shutdown,
                worker: Some(worker),
                keep_alive: Some(keep_alive),
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dnd::dropfiles::DROPFILES_HEADER_SIZE;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    fn make_dropfiles_wide(paths: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(DROPFILES_HEADER_SIZE as u32).to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes()); // pt.x
        out.extend_from_slice(&0i32.to_le_bytes()); // pt.y
        out.extend_from_slice(&0u32.to_le_bytes()); // fNC
        out.extend_from_slice(&1u32.to_le_bytes()); // fWide = TRUE
        for path in paths {
            for unit in path.encode_utf16() {
                out.extend_from_slice(&unit.to_le_bytes());
            }
            out.extend_from_slice(&0u16.to_le_bytes());
        }
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    // ─── dispatch_dropfiles_to_injector — existing tests ───────────────

    #[test]
    fn dispatch_forwards_parsed_paths_to_injector() {
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = Arc::clone(&captured);
        let injector: DropInjector = Box::new(move |paths: &[String]| {
            captured_clone.lock().unwrap().extend_from_slice(paths);
        });
        let bytes = make_dropfiles_wide(&[r"C:\test\a.txt", r"C:\test\b.txt"]);

        dispatch_dropfiles_to_injector(&bytes, &injector);

        let got = captured.lock().unwrap().clone();
        assert_eq!(got, vec![r"C:\test\a.txt", r"C:\test\b.txt"]);
    }

    #[test]
    fn dispatch_with_empty_buffer_does_not_invoke_injector() {
        let calls: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let calls_clone = Arc::clone(&calls);
        let injector: DropInjector = Box::new(move |_paths: &[String]| {
            *calls_clone.lock().unwrap() += 1;
        });

        dispatch_dropfiles_to_injector(&[], &injector);

        assert_eq!(*calls.lock().unwrap(), 0);
    }

    #[test]
    fn dispatch_with_malformed_buffer_does_not_invoke_injector() {
        // Truncated header — parse_dropfiles_buffer returns empty,
        // so the injector must not fire (avoids a zero-path "drop").
        let calls: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let calls_clone = Arc::clone(&calls);
        let injector: DropInjector = Box::new(move |_paths: &[String]| {
            *calls_clone.lock().unwrap() += 1;
        });

        let truncated = vec![0u8; DROPFILES_HEADER_SIZE - 1];
        dispatch_dropfiles_to_injector(&truncated, &injector);

        assert_eq!(*calls.lock().unwrap(), 0);
    }

    #[test]
    fn dispatch_normalizes_paths_before_injection() {
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = Arc::clone(&captured);
        let injector: DropInjector = Box::new(move |paths: &[String]| {
            captured_clone.lock().unwrap().extend_from_slice(paths);
        });

        let bytes = make_dropfiles_wide(&[r"C:\Users\me\Документы\file.txt"]);
        dispatch_dropfiles_to_injector(&bytes, &injector);

        let got = captured.lock().unwrap().clone();
        assert_eq!(got, vec![r"C:\Users\me\Документы\file.txt"]);
    }

    #[test]
    fn unsupported_platform_or_not_implemented_for_windows() {
        // Smoke test of the public API. On non-Windows hosts we get
        // UnsupportedPlatform; on Windows the unit-test process may
        // or may not have a console (CI runners typically don't),
        // so we just assert that the call returns *some* result
        // without panicking.
        let injector: DropInjector = Box::new(|_| {});
        let _ = register_console_drop_target(injector, RefreshConfig::immediate_no_refresh());
    }

    // ─── RefreshConfig shape ───────────────────────────────────────────

    #[test]
    fn refresh_config_default_uses_2s_initial_3s_refresh() {
        let cfg = RefreshConfig::default_displacement();
        assert_eq!(cfg.initial_delay, Duration::from_secs(2));
        assert_eq!(cfg.refresh_interval, Duration::from_secs(3));
    }

    #[test]
    fn refresh_config_immediate_no_refresh_zero() {
        let cfg = RefreshConfig::immediate_no_refresh();
        assert_eq!(cfg.initial_delay, Duration::ZERO);
        assert_eq!(cfg.refresh_interval, Duration::ZERO);
    }

    #[test]
    fn refresh_config_default_trait_matches_displacement() {
        let a = RefreshConfig::default();
        let b = RefreshConfig::default_displacement();
        assert_eq!(a.initial_delay, b.initial_delay);
        assert_eq!(a.refresh_interval, b.refresh_interval);
    }

    // ─── Worker-loop behavior via MockRegistrar ────────────────────────

    struct MockRegistrar {
        register_calls: Arc<AtomicUsize>,
        fail_initial: bool,
    }

    impl DragDropRegistrar for MockRegistrar {
        fn register(&self) -> Result<(), i32> {
            let n = self.register_calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_initial && n == 0 {
                return Err(0x8000_4005u32 as i32); // E_FAIL
            }
            Ok(())
        }
        fn revoke(&self) -> Result<(), i32> {
            Ok(())
        }
    }

    #[test]
    fn registration_loop_calls_register_once_when_refresh_disabled() {
        let registrar = MockRegistrar {
            register_calls: Arc::new(AtomicUsize::new(0)),
            fail_initial: false,
        };
        let calls = Arc::clone(&registrar.register_calls);
        let shutdown = RefreshShutdown::new();
        run_registration_loop(&registrar, RefreshConfig::immediate_no_refresh(), &shutdown)
            .expect("ok");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn registration_loop_surfaces_initial_failure() {
        let registrar = MockRegistrar {
            register_calls: Arc::new(AtomicUsize::new(0)),
            fail_initial: true,
        };
        let shutdown = RefreshShutdown::new();
        let err =
            run_registration_loop(&registrar, RefreshConfig::immediate_no_refresh(), &shutdown)
                .expect_err("must surface initial register failure");
        assert_eq!(err, 0x8000_4005u32 as i32);
    }

    #[test]
    fn registration_loop_refreshes_periodically() {
        let registrar = Arc::new(MockRegistrar {
            register_calls: Arc::new(AtomicUsize::new(0)),
            fail_initial: false,
        });
        let calls = Arc::clone(&registrar.register_calls);
        let shutdown = Arc::new(RefreshShutdown::new());
        let cfg = RefreshConfig {
            initial_delay: Duration::ZERO,
            refresh_interval: Duration::from_millis(50),
        };

        let worker_shutdown = Arc::clone(&shutdown);
        let worker_registrar = Arc::clone(&registrar);
        let handle = std::thread::spawn(move || {
            run_registration_loop(&*worker_registrar, cfg, &worker_shutdown)
        });

        // Let the loop tick a few times.
        std::thread::sleep(Duration::from_millis(220));
        shutdown.signal();
        let result = handle.join().expect("worker panicked");
        result.expect("loop ok");

        // 1 initial + at least 2 refreshes within ~220ms.
        let n = calls.load(Ordering::SeqCst);
        assert!(
            n >= 3,
            "expected at least 3 register calls, got {} (initial + ≥2 refreshes)",
            n
        );
    }

    #[test]
    fn registration_loop_exits_promptly_on_shutdown_during_initial_delay() {
        let registrar = Arc::new(MockRegistrar {
            register_calls: Arc::new(AtomicUsize::new(0)),
            fail_initial: false,
        });
        let calls = Arc::clone(&registrar.register_calls);
        let shutdown = Arc::new(RefreshShutdown::new());
        let cfg = RefreshConfig {
            initial_delay: Duration::from_secs(60), // way longer than the test waits
            refresh_interval: Duration::from_secs(60),
        };

        let worker_shutdown = Arc::clone(&shutdown);
        let worker_registrar = Arc::clone(&registrar);
        let handle = std::thread::spawn(move || {
            run_registration_loop(&*worker_registrar, cfg, &worker_shutdown)
        });

        // Signal shutdown during the initial-delay phase.
        std::thread::sleep(Duration::from_millis(50));
        shutdown.signal();

        let start = std::time::Instant::now();
        let _ = handle.join().expect("worker panicked");
        let elapsed = start.elapsed();

        // Worker should exit within 500ms (responsive_sleep wakes
        // every 100ms).
        assert!(
            elapsed < Duration::from_millis(500),
            "worker took {:?} to exit after shutdown",
            elapsed
        );
        // Register should NOT have been called — we shut down
        // before the initial delay elapsed.
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn responsive_sleep_returns_false_when_shutdown_already_signaled() {
        let shutdown = RefreshShutdown::new();
        shutdown.signal();
        // Already-signaled shutdown — should return false even
        // with zero duration or a very long one.
        assert!(!responsive_sleep(Duration::ZERO, &shutdown));
        assert!(!responsive_sleep(Duration::from_secs(60), &shutdown));
    }

    #[test]
    fn responsive_sleep_returns_true_for_zero_duration_no_shutdown() {
        let shutdown = RefreshShutdown::new();
        assert!(responsive_sleep(Duration::ZERO, &shutdown));
    }
}
