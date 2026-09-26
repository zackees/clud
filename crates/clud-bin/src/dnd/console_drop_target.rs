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
        #[cfg(windows)]
        pump_ole_messages();
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
        #[cfg(windows)]
        {
            pump_ole_messages();
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::MsgWaitForMultipleObjects(
                    None,
                    false,
                    step.as_millis().min(u128::from(u32::MAX)) as u32,
                    windows::Win32::UI::WindowsAndMessaging::QS_ALLINPUT,
                );
            }
            pump_ole_messages();
        }
        #[cfg(not(windows))]
        std::thread::sleep(step);
        remaining = remaining.saturating_sub(step);
    }
    !shutdown.is_signaled()
}

#[cfg(windows)]
fn pump_ole_messages() {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };

    let mut msg = MSG::default();
    while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() } {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
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
#[cfg(any(windows, test))]
const DROPEFFECT_NONE_BITS: u32 = 0;
/// `DROPEFFECT_COPY` (oleidl.h); see [`DROPEFFECT_NONE_BITS`].
#[cfg(any(windows, test))]
const DROPEFFECT_COPY_BITS: u32 = 1;

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
#[cfg(any(windows, test))]
fn drag_effect(source_allowed: u32, carries_files: bool) -> u32 {
    if carries_files && source_allowed & DROPEFFECT_COPY_BITS != 0 {
        DROPEFFECT_COPY_BITS
    } else {
        DROPEFFECT_NONE_BITS
    }
}

// ─── Windows-only implementation ──────────────────────────────────────

#[cfg(windows)]
mod win {
    use super::*;
    use crate::dnd::drop_host::{resolve_drop_host, DropHost, ProcessEntry};
    use std::sync::Mutex;

    use windows::core::ComObject;
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINTL, RPC_E_CHANGED_MODE};
    use windows::Win32::System::Com::IDataObject;
    use windows::Win32::System::Console::GetConsoleWindow;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Ole::{
        IDropTarget, IDropTarget_Impl, OleInitialize, OleUninitialize, RegisterDragDrop,
        RevokeDragDrop, DROPEFFECT,
    };
    use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsWindowVisible,
    };

    /// COM object implementing `IDropTarget`. Advertises
    /// `DROPEFFECT_COPY` for a file drag (so the cursor switches from ⊘
    /// to a copy indicator) and `DROPEFFECT_NONE` for anything else,
    /// per [`super::drag_effect`]; on `Drop()` it extracts the
    /// `CF_HDROP` bytes and hands them to
    /// [`super::dispatch_dropfiles_to_injector`].
    #[windows::core::implement(IDropTarget)]
    struct ConsoleDropTarget {
        injector: Mutex<DropInjector>,
        /// Whether the data object seen by the current `DragEnter`
        /// offers `CF_HDROP`. `DragOver` gets no data object, so it
        /// reuses this; `DragLeave` and `Drop` clear it.
        carries_files: AtomicBool,
    }

    /// Build the `IDropTarget` clud registers. Split out of
    /// [`register`] so tests can drive the real COM vtable without a
    /// console window or `RegisterDragDrop` (#1362).
    pub(super) fn new_drop_target(injector: DropInjector) -> IDropTarget {
        ComObject::new(ConsoleDropTarget {
            injector: Mutex::new(injector),
            carries_files: AtomicBool::new(false),
        })
        .to_interface::<IDropTarget>()
    }

    /// The effects the drag source allows, read from the in/out
    /// `pdwEffect`. A null pointer violates the `IDropTarget` contract;
    /// treat it as "copy allowed" (the result cannot be written back
    /// anyway).
    ///
    /// # Safety
    ///
    /// `effect` is null or points to a readable `DROPEFFECT`.
    unsafe fn source_allowed(effect: *const DROPEFFECT) -> u32 {
        if effect.is_null() {
            super::DROPEFFECT_COPY_BITS
        } else {
            // SAFETY: non-null per the check above; caller guarantees
            // it is readable.
            unsafe { (*effect).0 }
        }
    }

    /// # Safety
    ///
    /// `effect` is null or points to a writable `DROPEFFECT`.
    unsafe fn write_effect(effect: *mut DROPEFFECT, bits: u32) {
        if !effect.is_null() {
            // SAFETY: non-null per the check above; caller guarantees
            // it is writable.
            unsafe { *effect = DROPEFFECT(bits) };
        }
    }

    impl ConsoleDropTarget {
        /// Deliver a dropped payload. Returns whether the injector was
        /// called with at least one path.
        fn deliver(&self, data: &IDataObject) -> bool {
            // SAFETY: `data` is a live interface borrowed from OLE for
            // the duration of `Drop`; STGMEDIUM bookkeeping is handled
            // inside.
            let Some(buf) = (unsafe { copy_cf_hdrop_bytes(data) }) else {
                return false;
            };
            let Ok(guard) = self.injector.lock() else {
                return false;
            };
            // A panic must not unwind across the COM boundary. The
            // guard is held outside the closure, so a panicking
            // injector does not poison the mutex for the next drop.
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                super::dispatch_dropfiles_to_injector(&buf, &guard)
            }))
            .unwrap_or(false)
        }
    }

    #[allow(non_snake_case)]
    impl IDropTarget_Impl for ConsoleDropTarget_Impl {
        fn DragEnter(
            &self,
            data: windows_core::Ref<'_, IDataObject>,
            _key_state: MODIFIERKEYS_FLAGS,
            _pt: &POINTL,
            effect: *mut DROPEFFECT,
        ) -> windows_core::Result<()> {
            let carries_files = data.as_ref().is_some_and(offers_cf_hdrop);
            self.carries_files.store(carries_files, Ordering::SeqCst);
            // SAFETY: `effect` is the in/out pointer OLE passes per the
            // IDropTarget contract.
            unsafe {
                let bits = super::drag_effect(source_allowed(effect), carries_files);
                write_effect(effect, bits);
            }
            Ok(())
        }

        fn DragOver(
            &self,
            _key_state: MODIFIERKEYS_FLAGS,
            _pt: &POINTL,
            effect: *mut DROPEFFECT,
        ) -> windows_core::Result<()> {
            let carries_files = self.carries_files.load(Ordering::SeqCst);
            // SAFETY: see DragEnter.
            unsafe {
                let bits = super::drag_effect(source_allowed(effect), carries_files);
                write_effect(effect, bits);
            }
            Ok(())
        }

        fn DragLeave(&self) -> windows_core::Result<()> {
            self.carries_files.store(false, Ordering::SeqCst);
            Ok(())
        }

        fn Drop(
            &self,
            data: windows_core::Ref<'_, IDataObject>,
            _key_state: MODIFIERKEYS_FLAGS,
            _pt: &POINTL,
            effect: *mut DROPEFFECT,
        ) -> windows_core::Result<()> {
            self.carries_files.store(false, Ordering::SeqCst);
            // SAFETY: see DragEnter.
            let allowed = unsafe { source_allowed(effect) };
            // A source that forbids a copy gets nothing injected: the
            // drop is refused, so it must have no side effect either.
            let delivered = allowed & super::DROPEFFECT_COPY_BITS != 0
                && data.as_ref().is_some_and(|data| self.deliver(data));
            // SAFETY: see DragEnter.
            unsafe { write_effect(effect, super::drag_effect(allowed, delivered)) };
            Ok(())
        }
    }

    /// The one format clud reads: `CF_HDROP` in an `HGLOBAL`.
    fn cf_hdrop_format() -> windows::Win32::System::Com::FORMATETC {
        use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
        use windows::Win32::System::Ole::CF_HDROP;

        FORMATETC {
            cfFormat: CF_HDROP.0,
            ptd: core::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        }
    }

    /// Whether `data` can render [`cf_hdrop_format`]. Only `S_OK`
    /// counts: some data objects answer `S_FALSE` for "not available".
    fn offers_cf_hdrop(data: &IDataObject) -> bool {
        let format = cf_hdrop_format();
        // SAFETY: `data` is a live interface; `format` outlives the call.
        (unsafe { data.QueryGetData(&format) }) == windows::Win32::Foundation::S_OK
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
    pub(super) unsafe fn copy_cf_hdrop_bytes(data: &IDataObject) -> Option<Vec<u8>> {
        use windows::Win32::System::Com::TYMED_HGLOBAL;
        use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
        use windows::Win32::System::Ole::ReleaseStgMedium;

        let format = cf_hdrop_format();

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
        hwnds: Vec<SendHwnd>,
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
            let mut first_error: Option<i32> = None;
            let mut successes = 0usize;

            for hwnd in &self.hwnds {
                // SAFETY: pure FFI; hwnds are probed before construction.
                // Revoke first so clud can replace a terminal/backend target
                // while this foreground session owns the interaction.
                let _ = unsafe { RevokeDragDrop(hwnd.0) };

                // SAFETY: pure FFI; self.target is a ref-counted COM object.
                match unsafe { RegisterDragDrop(hwnd.0, &self.target) } {
                    Ok(()) => successes += 1,
                    Err(e) => {
                        first_error.get_or_insert(e.code().0);
                    }
                }
            }

            if successes > 0 {
                Ok(())
            } else {
                Err(first_error.unwrap_or(0x8000_4005u32 as i32))
            }
        }

        fn revoke(&self) -> Result<(), i32> {
            let mut first_error: Option<i32> = None;
            for hwnd in &self.hwnds {
                // SAFETY: pure FFI; best-effort cleanup for every target.
                if let Err(e) = unsafe { RevokeDragDrop(hwnd.0) } {
                    first_error.get_or_insert(e.code().0);
                }
            }
            if let Some(error) = first_error {
                Err(error)
            } else {
                Ok(())
            }
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
            // Normal cleanup happens on the worker STA thread. This is
            // only a fallback for an abnormal worker exit after OLE init.
            if self.registrar.ole_initialized.swap(false, Ordering::SeqCst) {
                // Best-effort cleanup — errors here can't be surfaced.
                let _ = self.registrar.revoke();
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
        // 1. Resolve every HWND that can receive the Explorer drag.
        let hwnds = drop_target_hwnds();
        if hwnds.is_empty() {
            return Err(RegisterError::ConsoleWindowUnavailable);
        }

        // 2. Construct the IDropTarget COM object up-front. This is
        //    cheap and lets us hand a single `Arc<OleRegistrar>`
        //    into the worker thread.
        let target_iface = new_drop_target(injector);

        let registrar = Arc::new(OleRegistrar {
            hwnds: hwnds.into_iter().map(SendHwnd).collect(),
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
                // Cleanup belongs on this STA thread: it is the one
                // that called OleInitialize and RegisterDragDrop.
                let _ = worker_registrar.revoke();
                if worker_registrar
                    .ole_initialized
                    .swap(false, Ordering::SeqCst)
                {
                    // SAFETY: balanced against OleInitialize above.
                    unsafe { OleUninitialize() };
                }
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

    fn drop_target_hwnds() -> Vec<HWND> {
        let mut hwnds = Vec::new();

        let env = |name: &str| std::env::var(name).ok();
        // SAFETY: pure FFI, no preconditions.
        let current_pid = unsafe { GetCurrentProcessId() };
        if let DropHost::WindowsTerminal { pid } =
            resolve_drop_host(&env, current_pid, process_entries)
        {
            for hwnd in visible_top_level_windows_for_pid(pid) {
                push_unique_hwnd(&mut hwnds, hwnd);
            }
        }

        // SAFETY: pure FFI, no preconditions.
        let console = unsafe { GetConsoleWindow() };
        push_unique_hwnd(&mut hwnds, console);
        hwnds
    }

    fn push_unique_hwnd(hwnds: &mut Vec<HWND>, hwnd: HWND) {
        if hwnd.is_invalid() {
            return;
        }
        if hwnds.iter().any(|existing| existing.0 == hwnd.0) {
            return;
        }
        hwnds.push(hwnd);
    }

    fn process_entries() -> Vec<ProcessEntry> {
        // SAFETY: pure FFI snapshot.
        let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
            return Vec::new();
        };

        let mut entries = Vec::new();
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        // SAFETY: `entry` has the required dwSize field initialized.
        if unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok() {
            loop {
                entries.push(ProcessEntry {
                    pid: entry.th32ProcessID,
                    parent_pid: entry.th32ParentProcessID,
                    exe: nul_terminated_wide_to_string(&entry.szExeFile),
                });

                // SAFETY: same initialized PROCESSENTRY32W buffer.
                if unsafe { Process32NextW(snapshot, &mut entry) }.is_err() {
                    break;
                }
            }
        }

        // SAFETY: closes the snapshot handle returned above.
        let _ = unsafe { CloseHandle(snapshot) };
        entries
    }

    fn nul_terminated_wide_to_string(buf: &[u16]) -> String {
        let len = buf.iter().position(|&unit| unit == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    }

    struct EnumWindowsState {
        pid: u32,
        hwnds: Vec<HWND>,
    }

    fn visible_top_level_windows_for_pid(pid: u32) -> Vec<HWND> {
        let mut state = EnumWindowsState {
            pid,
            hwnds: Vec::new(),
        };
        let state_ptr = &mut state as *mut EnumWindowsState;
        // SAFETY: callback only uses `state_ptr` during this synchronous call.
        let _ = unsafe { EnumWindows(Some(enum_windows_proc), LPARAM(state_ptr as isize)) };
        state.hwnds
    }

    unsafe extern "system" fn enum_windows_proc(hwnd: HWND, lparam: LPARAM) -> windows_core::BOOL {
        let state = unsafe { &mut *(lparam.0 as *mut EnumWindowsState) };
        if unsafe { IsWindowVisible(hwnd).as_bool() } {
            let mut pid = 0u32;
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
            }
            if pid == state.pid {
                push_unique_hwnd(&mut state.hwnds, hwnd);
            }
        }
        windows_core::BOOL(1)
    }

    #[cfg(test)]
    mod win_tests {
        use super::*;

        /// The one test that runs the real Toolhelp snapshot the host
        /// decision consumes; the decision itself is covered on every
        /// host in `drop_host_tests.rs`.
        #[test]
        fn real_process_snapshot_contains_this_process() {
            // SAFETY: pure FFI, no preconditions.
            let current_pid = unsafe { GetCurrentProcessId() };
            let entries = process_entries();
            let me = entries
                .iter()
                .find(|entry| entry.pid == current_pid)
                .expect("the Toolhelp snapshot must list the test process itself");
            assert!(
                me.exe.to_ascii_lowercase().ends_with(".exe"),
                "unexpected exe name for the test process: {:?}",
                me.exe
            );

            // Without WT_SESSION or TERM_PROGRAM the snapshot is not needed.
            let host = resolve_drop_host(&|_: &str| None, current_pid, process_entries);
            assert_eq!(host, DropHost::ConsoleWindowOnly);
        }
    }
}

#[cfg(test)]
#[path = "console_drop_target_tests.rs"]
mod tests;

/// The real COM layer: the `IDropTarget` vtable driven with a fake
/// `IDataObject`, and the `CF_HDROP` `HGLOBAL` extraction (#1362).
#[cfg(all(test, windows))]
#[path = "console_drop_target_com_tests.rs"]
mod com_tests;
