//! Windows-only console drag-drop target adapter (skeleton).
//!
//! ## Why this exists
//!
//! Issue #65: dragging a file onto the console window running `clud` on
//! Windows produces the OS "no-drop" cursor — the drop is rejected at the
//! OLE layer (`IDropTarget::DragEnter` → `DROPEFFECT_NONE`) before any
//! bytes reach `clud`'s stdin. The fix path described in #66 is to have
//! `clud` register **its own** `IDropTarget` on `GetConsoleWindow()` so
//! the most-recent-registration-wins rule of `RegisterDragDrop`
//! displaces conhost's refusal.
//!
//! ## What's here
//!
//! Per the scoping in #66 ("Land the testable kernel only…"), this
//! module is the **skeleton** for the adapter:
//!
//! - The public registration function and RAII guard with their final
//!   shape and error contract.
//! - The platform-agnostic *dispatch* half — given a raw `CF_HDROP`
//!   byte buffer, decode it via [`super::dropfiles::parse_dropfiles_buffer`],
//!   normalize each path via [`super::normalize_dropped_path`], and
//!   forward the result to the caller-supplied injector. Fully unit-
//!   tested on every CI host.
//! - The Windows-side OLE plumbing (`OleInitialize`, the `IDropTarget`
//!   vtable, `RegisterDragDrop(GetConsoleWindow(), …)`) is **not yet
//!   wired**; [`register_console_drop_target`] returns
//!   [`RegisterError::NotImplemented`] until the follow-up PR lands.
//!
//! ## What's not here
//!
//! - Wiring into `main.rs` — that needs to know the launch mode (PTY vs
//!   subprocess) and which handle to inject the bytes into. Tracked in
//!   #66.
//! - The byte injectors themselves (`WriteConsoleInputW` for subprocess
//!   mode, PTY-master writes for PTY mode). The injector closure here is
//!   the seam those will plug into.

use crate::dnd::dropfiles::parse_dropfiles_buffer;
use crate::dnd::normalize_dropped_path;

/// Caller-supplied callback invoked once per drop, with the list of
/// parsed-and-normalized file paths from a `CF_HDROP` payload.
///
/// Implementations should be cheap and non-blocking — the callback runs
/// inline on the OLE drop notification, which on Windows is delivered on
/// the thread that owns the console window.
pub type DropInjector = Box<dyn Fn(&[String]) + Send + Sync + 'static>;

/// Failure modes for [`register_console_drop_target`].
#[derive(Debug)]
pub enum RegisterError {
    /// The Windows OLE wiring hasn't landed yet (see module-level docs
    /// and issue #66). The platform-agnostic dispatch path is callable
    /// in the meantime via [`dispatch_dropfiles_to_injector`].
    NotImplemented,
    /// `register_console_drop_target` was called from a non-Windows
    /// build. The whole subsystem is Windows-only by design — POSIX
    /// terminals already deliver drops as stdin bytes that the #63
    /// normalizer handles.
    UnsupportedPlatform,
    /// `GetConsoleWindow()` returned `NULL` — `clud` is not attached to
    /// a console (e.g. running detached or under a service host). No
    /// drop target is possible.
    ConsoleWindowUnavailable,
    /// `OleInitialize` failed with the given `HRESULT`.
    OleInitializeFailed(i32),
    /// `RegisterDragDrop` failed with the given `HRESULT`. Common cause:
    /// a different process already owns the drop target, or UIPI is
    /// blocking the call.
    RegisterDragDropFailed(i32),
}

impl std::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegisterError::NotImplemented => f.write_str(
                "console IDropTarget skeleton is in place but COM wiring is pending (#66)",
            ),
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
        }
    }
}

impl std::error::Error for RegisterError {}

/// RAII guard returned by a successful registration. Dropping it calls
/// `RevokeDragDrop` + `OleUninitialize` so the console window is left in
/// the same state it was found in.
///
/// The guard is intentionally opaque — its internals will hold the COM
/// pointer and HWND once the implementation lands.
pub struct ConsoleDropTargetGuard {
    // Held private so future fields (HWND, IDropTarget pointer,
    // OleUninitialize cookie) can be added without a breaking API
    // change.
    _private: (),
}

impl Drop for ConsoleDropTargetGuard {
    fn drop(&mut self) {
        // TODO(#66): RevokeDragDrop(self.hwnd); OleUninitialize();
    }
}

/// Register `clud` as the `IDropTarget` for the console window so dropped
/// files are routed to `injector` after parsing and normalization.
///
/// Currently returns [`RegisterError::NotImplemented`] on Windows — the
/// COM plumbing lands in a follow-up PR (see #66). Already wired to
/// return [`RegisterError::UnsupportedPlatform`] on POSIX so callers can
/// build cross-platform without target-cfg gates.
#[cfg(windows)]
pub fn register_console_drop_target(
    _injector: DropInjector,
) -> Result<ConsoleDropTargetGuard, RegisterError> {
    // Implementation outline (lands in the follow-up):
    //
    //   1. let hwnd = GetConsoleWindow();
    //      if hwnd.is_invalid() { return Err(ConsoleWindowUnavailable); }
    //   2. OleInitialize(None) — bail with OleInitializeFailed(hr) on err.
    //   3. Construct an IDropTarget COM object whose Drop() callback
    //      pulls CF_HDROP from the IDataObject, then runs the buffer
    //      through `dispatch_dropfiles_to_injector(&buf, &injector)`.
    //   4. RegisterDragDrop(hwnd, &drop_target) — bail with
    //      RegisterDragDropFailed(hr) on err; on success, store hwnd in
    //      the guard so Drop can RevokeDragDrop it.
    //
    // The dispatch callback is already implemented and tested below;
    // only the COM wiring is missing.
    Err(RegisterError::NotImplemented)
}

#[cfg(not(windows))]
pub fn register_console_drop_target(
    _injector: DropInjector,
) -> Result<ConsoleDropTargetGuard, RegisterError> {
    Err(RegisterError::UnsupportedPlatform)
}

/// Platform-agnostic glue: decode a `CF_HDROP` byte buffer, normalize
/// each path via [`normalize_dropped_path`], and forward the list to the
/// injector.
///
/// Exposed as a separate function (rather than buried inside the COM
/// callback) so it can be unit-tested on every CI host. The Windows
/// `IDropTarget::Drop` implementation in the follow-up will be a thin
/// wrapper around this — extract the `CF_HDROP` HGLOBAL bytes, then
/// hand them here.
pub fn dispatch_dropfiles_to_injector(buf: &[u8], injector: &DropInjector) {
    let parsed = parse_dropfiles_buffer(buf);
    if parsed.is_empty() {
        return;
    }
    let normalized: Vec<String> = parsed.iter().map(|p| normalize_dropped_path(p)).collect();
    injector(&normalized);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dnd::dropfiles::DROPFILES_HEADER_SIZE;
    use std::sync::{Arc, Mutex};

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
        // Truncated header — parse_dropfiles_buffer returns empty, so
        // the injector must not fire (avoids a zero-path "drop" event).
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
        // CF_HDROP normally carries already-canonical Windows paths, but
        // the contract is that we always run normalize_dropped_path so
        // the injector receives the same canonical shape regardless of
        // whether bytes came from OLE or stdin (#63 path).
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = Arc::clone(&captured);
        let injector: DropInjector = Box::new(move |paths: &[String]| {
            captured_clone.lock().unwrap().extend_from_slice(paths);
        });

        // Input that already looks canonical on Windows; normalizer is a
        // no-op. The test guards against accidentally double-quoting or
        // dropping bytes during the dispatch step.
        let bytes = make_dropfiles_wide(&[r"C:\Users\me\Документы\file.txt"]);
        dispatch_dropfiles_to_injector(&bytes, &injector);

        let got = captured.lock().unwrap().clone();
        assert_eq!(got, vec![r"C:\Users\me\Документы\file.txt"]);
    }

    #[test]
    fn unsupported_platform_or_not_implemented_for_windows() {
        // Smoke test that the public API at least returns *some* error
        // until the COM wiring lands. We don't pin which variant — the
        // intent is to ensure registration is a no-op until #66's
        // follow-up.
        let injector: DropInjector = Box::new(|_| {});
        let result = register_console_drop_target(injector);
        assert!(result.is_err());
    }
}
