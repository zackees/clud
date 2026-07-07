//! Windows console-input translator (issue #141).
//!
//! Conhost strips modifier-key state before producing the `\r` byte on
//! stdin, so reading the byte stream can't distinguish Enter from
//! Shift+Enter. `ReadConsoleInputW` exposes the modifier state via
//! `KEY_EVENT_RECORD::dwControlKeyState`, which is what this module
//! consumes.
//!
//! Scope: just the pure-function translator from a slice of
//! `InputEvent`s to the bytes that should be forwarded into the child
//! PTY's stdin. The thread that calls `ReadConsoleInputW` and the
//! console-mode plumbing land in a follow-up PR.
//!
//! Translation rules:
//!
//! - `VK_RETURN` key-down with `SHIFT_PRESSED` set in `control_key_state`
//!   emits `\n` (0x0a) — a literal newline the backend treats as
//!   "insert newline in the prompt."
//! - `VK_RETURN` key-down without `SHIFT_PRESSED` emits `\r` (0x0d) —
//!   the usual "submit" byte. Other Enter-modifier combinations
//!   (Ctrl+Enter, Alt+Enter) currently fall through to plain `\r` so we
//!   don't quietly change the behavior of binds the backend may use.
//! - Any other key-down with a non-zero `unicode_char` emits the
//!   UTF-8 encoding of that UTF-16 code unit. This handles plain ASCII
//!   typing without needing a dedicated keyboard layout decoder.
//! - Key-up events are dropped.
//! - Non-key input records (mouse / focus / buffer-size / menu / window
//!   events from `ReadConsoleInputW`) are dropped at the
//!   `InputEvent::NonKey` variant.
//!
//! ## Wiring (PR follow-up to #142)
//!
//! [`spawn_console_input_reader`] returns a [`ConsoleInputHandle`]
//! containing a receiver the PTY pump can plug into its `extra_rx`
//! slot. A background thread calls `ReadConsoleInputW`, runs the
//! events through [`translate`], and forwards the resulting bytes.
//! [`ConsoleModeGuard`] saves/restores the console mode bits so the
//! reader gets raw key events while it runs and the terminal returns
//! to its previous state on Drop.
//!
//! [`spawn_with_source`] is the testable seam: tests can substitute
//! any `FnMut() -> Vec<InputEvent>` for the real `ReadConsoleInputW`
//! call so the worker can be exercised without an actual console.

#![cfg(windows)]

/// Windows virtual-key code for the Enter / Return key.
/// Mirrors `winuser.h`'s `VK_RETURN` so the test module doesn't need a
/// link-time dependency on `windows-sys`.
pub const VK_RETURN: u16 = 0x0D;

/// Windows virtual-key code for V. Ctrl+V is the clipboard-paste chord.
pub const VK_V: u16 = 0x56;

/// `KEY_EVENT_RECORD::dwControlKeyState` bit for Left- or Right-Shift.
/// Mirrors `wincon.h`'s `SHIFT_PRESSED`.
pub const SHIFT_PRESSED: u32 = 0x0010;

/// `KEY_EVENT_RECORD::dwControlKeyState` bits for Left- or Right-Ctrl.
pub const CTRL_PRESSED: u32 = 0x0004 | 0x0008;

/// Subset of `KEY_EVENT_RECORD` the translator needs. Tests construct
/// these directly without going through Windows APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    /// True for `KEY_EVENT_RECORD::bKeyDown == TRUE` (a press, including
    /// autorepeat). The translator ignores key-up events.
    pub key_down: bool,
    /// `KEY_EVENT_RECORD::wVirtualKeyCode`.
    pub virtual_key_code: u16,
    /// `KEY_EVENT_RECORD::uChar.UnicodeChar` (a single UTF-16 code unit).
    /// Zero when the key has no character mapping (function keys, etc.).
    pub unicode_char: u16,
    /// `KEY_EVENT_RECORD::dwControlKeyState` bitfield.
    pub control_key_state: u32,
}

/// One `INPUT_RECORD` from `ReadConsoleInputW`. Non-key records carry
/// no bytes through to the PTY, so we collapse them all into a single
/// `NonKey` variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    Key(KeyEvent),
    /// Mouse / focus / buffer-size / menu / window event.
    NonKey,
}

/// Translate a slice of input events into the bytes that should be
/// written to the child PTY's stdin. See the module-level docstring
/// for the translation rules.
pub fn translate(events: &[InputEvent]) -> Vec<u8> {
    translate_with_clipboard(events, || {
        crate::paste_image::handle_clipboard().ok().flatten()
    })
}

pub fn translate_with_clipboard<F>(events: &[InputEvent], mut handle_clipboard: F) -> Vec<u8>
where
    F: FnMut() -> Option<Vec<u8>>,
{
    let mut out = Vec::new();
    for event in events {
        let key = match event {
            InputEvent::Key(k) if k.key_down => k,
            // Key-up events and non-key records both drop here.
            _ => continue,
        };
        if key.virtual_key_code == VK_RETURN {
            let shift = (key.control_key_state & SHIFT_PRESSED) != 0;
            out.push(if shift { b'\n' } else { b'\r' });
            continue;
        }
        if key.virtual_key_code == VK_V && (key.control_key_state & CTRL_PRESSED) != 0 {
            if let Some(bytes) = handle_clipboard() {
                out.extend_from_slice(&bytes);
                continue;
            }
        }
        // Regular char key. `unicode_char` is one UTF-16 code unit; for
        // BMP keys that's the full character. Surrogate pairs from
        // dead-key sequences arrive as two consecutive events, which
        // `from_utf16_lossy` joins correctly if both halves end up in
        // the same `translate` call.
        if key.unicode_char != 0 {
            let s = String::from_utf16_lossy(&[key.unicode_char]);
            out.extend_from_slice(s.as_bytes());
        }
    }
    out
}

// ---------- INPUT_RECORD adapter ----------

use windows::Win32::System::Console::{
    FOCUS_EVENT, INPUT_RECORD, KEY_EVENT, MENU_EVENT, MOUSE_EVENT, WINDOW_BUFFER_SIZE_EVENT,
};

/// Convert one `windows` crate `INPUT_RECORD` into the simpler
/// [`InputEvent`] the translator works with. Returns `Some(NonKey)` for
/// every record type that isn't `KEY_EVENT` so the caller still observes
/// the event (useful for rate-limiting / diagnostics).
///
/// SAFETY: the inner union access is gated on `EventType`. The
/// `windows` crate marks the union access unsafe; we localize that
/// `unsafe` block here so every other call site stays safe.
pub fn from_input_record(record: &INPUT_RECORD) -> Option<InputEvent> {
    // `EventType` values are u16 constants from `Win32_System_Console`.
    let is_key = record.EventType == KEY_EVENT as u16;
    let is_other = matches!(
        record.EventType,
        x if x == MOUSE_EVENT as u16
            || x == WINDOW_BUFFER_SIZE_EVENT as u16
            || x == FOCUS_EVENT as u16
            || x == MENU_EVENT as u16
    );
    if !is_key && !is_other {
        return None;
    }
    if is_other {
        return Some(InputEvent::NonKey);
    }
    // SAFETY: we just verified EventType == KEY_EVENT, so reading the
    // KeyEvent union field is well-defined.
    let key = unsafe { record.Event.KeyEvent };
    Some(InputEvent::Key(KeyEvent {
        key_down: key.bKeyDown.as_bool(),
        virtual_key_code: key.wVirtualKeyCode,
        // The `windows` crate exposes the union as `uChar` with both
        // `UnicodeChar` and `AsciiChar` views — we always read the
        // Unicode view since `translate` expects a UTF-16 code unit.
        unicode_char: unsafe { key.uChar.UnicodeChar },
        control_key_state: key.dwControlKeyState,
    }))
}

// ---------- reader worker thread ----------

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

/// Handle returned from [`spawn_with_source`] / [`spawn_console_input_reader`].
///
/// Owns the receiver the PTY pump will read from, a cancel flag the
/// pump sets on shutdown, and the worker thread join handle. Dropping
/// the handle signals cancel and joins the worker.
#[derive(Debug)]
pub struct ConsoleInputHandle {
    rx: Option<mpsc::Receiver<Vec<u8>>>,
    cancel: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl ConsoleInputHandle {
    /// Take the receiver out so it can be plugged into the pump's
    /// `extra_rx`. After this returns `Some`, subsequent calls return
    /// `None`.
    pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<Vec<u8>>> {
        self.rx.take()
    }

    /// Signal the worker to stop. The Drop impl does this too; this
    /// method exists for callers that want to stop the worker without
    /// dropping the handle (rare).
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for ConsoleInputHandle {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(handle) = self.join.take() {
            // Best-effort join: the worker checks `cancel` between each
            // call to `read_events`, which may block up to ~100ms on
            // `WaitForSingleObject` in the production reader.
            let _ = handle.join();
        }
    }
}

/// Spawn a worker that calls `read_events` in a loop, runs each
/// returned slice through [`translate`], and sends non-empty byte
/// chunks on the returned channel.
///
/// Tests substitute a canned `read_events` closure; production callers
/// use [`spawn_console_input_reader`].
pub fn spawn_with_source<F>(mut read_events: F) -> ConsoleInputHandle
where
    F: FnMut() -> Vec<InputEvent> + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let join = thread::Builder::new()
        .name("clud-console-input".into())
        .spawn(move || {
            while !worker_cancel.load(Ordering::Relaxed) {
                let events = read_events();
                if events.is_empty() {
                    continue;
                }
                let bytes = translate(&events);
                if bytes.is_empty() {
                    continue;
                }
                if tx.send(bytes).is_err() {
                    // Receiver dropped — pump shut down; exit cleanly.
                    break;
                }
            }
        })
        .expect("spawn clud-console-input thread");
    ConsoleInputHandle {
        rx: Some(rx),
        cancel,
        join: Some(join),
    }
}

// ---------- production reader + console-mode guard ----------

use std::io;
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Console::{
    GetConsoleMode, GetStdHandle, ReadConsoleInputW, SetConsoleMode, CONSOLE_MODE,
    ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_VIRTUAL_TERMINAL_INPUT,
    ENABLE_WINDOW_INPUT, STD_INPUT_HANDLE,
};
use windows::Win32::System::Threading::WaitForSingleObject;

/// RAII guard that flips the console input mode into the raw-key-event
/// regime expected by the reader and restores the previous mode on
/// Drop. The guard MUST outlive any [`ConsoleInputHandle`] that wraps
/// the production reader — dropping it while the worker is running
/// would leave the console in the wrong mode if the user's terminal
/// observes input directly between batches.
pub struct ConsoleModeGuard {
    handle: HANDLE,
    saved: CONSOLE_MODE,
}

impl ConsoleModeGuard {
    /// Set the console input mode for raw key-event reads.
    ///
    /// Toggles off `ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT |
    /// ENABLE_PROCESSED_INPUT | ENABLE_VIRTUAL_TERMINAL_INPUT` and
    /// turns on `ENABLE_WINDOW_INPUT`. Mouse input stays as-is — we
    /// don't need it but classifying mouse records as
    /// [`InputEvent::NonKey`] is cheap.
    pub fn set_raw() -> io::Result<Self> {
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) }
            .map_err(|e| io::Error::other(format!("GetStdHandle: {e}")))?;
        let mut saved = CONSOLE_MODE(0);
        unsafe { GetConsoleMode(handle, &mut saved) }
            .map_err(|e| io::Error::other(format!("GetConsoleMode: {e}")))?;
        let raw = (saved
            & !ENABLE_LINE_INPUT
            & !ENABLE_ECHO_INPUT
            & !ENABLE_PROCESSED_INPUT
            & !ENABLE_VIRTUAL_TERMINAL_INPUT)
            | ENABLE_WINDOW_INPUT;
        unsafe { SetConsoleMode(handle, raw) }
            .map_err(|e| io::Error::other(format!("SetConsoleMode: {e}")))?;
        Ok(Self { handle, saved })
    }
}

impl Drop for ConsoleModeGuard {
    fn drop(&mut self) {
        // Best-effort: a failure here would mean the terminal is left
        // in raw-input mode, which is annoying but recoverable by the
        // user (closing the terminal restores defaults).
        let _ = unsafe { SetConsoleMode(self.handle, self.saved) };
    }
}

/// Pull a batch of input events from STDIN. Blocks up to ~100ms in
/// `WaitForSingleObject` so the worker checks its cancel flag at that
/// cadence without busy-spinning.
fn read_console_input_w_batch(handle: HANDLE) -> Vec<InputEvent> {
    let wait = unsafe { WaitForSingleObject(handle, 100) };
    if wait != WAIT_OBJECT_0 {
        return Vec::new();
    }
    let mut buf = [INPUT_RECORD::default(); 32];
    let mut read: u32 = 0;
    let ok = unsafe { ReadConsoleInputW(handle, &mut buf, &mut read) };
    if ok.is_err() || read == 0 {
        return Vec::new();
    }
    buf[..read as usize]
        .iter()
        .filter_map(from_input_record)
        .collect()
}

/// Newtype wrapper that asserts `Send` for the Windows `HANDLE`. Raw
/// `HANDLE` (`*mut c_void`) is not `Send` in the type system, but a
/// kernel handle for STDIN is just an integer identifier the OS
/// dispatches per-thread — passing it to a worker thread is sound.
///
/// `Copy` is important: the production spawn closure uses `move` and
/// must capture the *whole* `SendHandle` (not the inner field via
/// disjoint-capture), otherwise the closure's capture set contains the
/// raw `*mut c_void` and the auto-Send analysis fails.
#[derive(Clone, Copy)]
struct SendHandle(HANDLE);
// SAFETY: kernel handles are thread-safe identifiers; the worker
// thread only uses this for `WaitForSingleObject` + `ReadConsoleInputW`
// against the process-shared stdin handle, which Windows guarantees is
// usable from any thread in the process.
unsafe impl Send for SendHandle {}

/// Production reader: set the console mode and spawn a worker that
/// drains `ReadConsoleInputW`. Returns the input handle plus a guard
/// that restores the original console mode on Drop. Both should live
/// for the duration of the PTY session.
pub fn spawn_console_input_reader() -> io::Result<(ConsoleInputHandle, ConsoleModeGuard)> {
    let guard = ConsoleModeGuard::set_raw()?;
    let thread_handle = SendHandle(guard.handle);
    let inner = spawn_with_source(move || {
        // Force the closure to capture the whole `SendHandle` (Send)
        // rather than its inner `*mut c_void` field via disjoint
        // capture. Binding via the wrapper inside the closure body
        // pins the capture at the wrapper level.
        let h = thread_handle;
        read_console_input_w_batch(h.0)
    });
    Ok((inner, guard))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `KEY_EVENT_RECORD::dwControlKeyState` bit for Left- or Right-Ctrl.
    /// Used only by the Ctrl+Enter test below — keeping it as a test-
    /// local constant so the public surface stays minimal until
    /// callers actually need the symbol.
    const CTRL_PRESSED: u32 = 0x0004 | 0x0008;

    fn key_down(vk: u16, ch: u16, ctrl_state: u32) -> InputEvent {
        InputEvent::Key(KeyEvent {
            key_down: true,
            virtual_key_code: vk,
            unicode_char: ch,
            control_key_state: ctrl_state,
        })
    }

    fn key_up(vk: u16, ch: u16, ctrl_state: u32) -> InputEvent {
        InputEvent::Key(KeyEvent {
            key_down: false,
            virtual_key_code: vk,
            unicode_char: ch,
            control_key_state: ctrl_state,
        })
    }

    /// Issue #141 case 1: plain Enter key-down emits `\r`. This is the
    /// "submit" byte the backend already keys on; the translator must
    /// not change this baseline.
    #[test]
    fn plain_enter_emits_carriage_return() {
        let events = [key_down(VK_RETURN, b'\r' as u16, 0)];
        assert_eq!(translate(&events), vec![b'\r']);
    }

    /// Issue #141 case 2: Shift+Enter emits `\n` — the whole point of
    /// the issue. A backend that's been told to treat `\n` as
    /// "insert newline" gets the literal newline without conhost or
    /// the terminal emulator needing any configuration.
    #[test]
    fn shift_enter_emits_line_feed() {
        let events = [key_down(VK_RETURN, b'\r' as u16, SHIFT_PRESSED)];
        assert_eq!(translate(&events), vec![b'\n']);
    }

    /// Issue #141 case 3: Ctrl+Enter falls through to plain `\r`. We
    /// only special-case Shift to avoid changing behavior of any
    /// existing Ctrl+Enter / Alt+Enter binding the backend may use.
    #[test]
    fn ctrl_enter_emits_carriage_return() {
        let events = [key_down(VK_RETURN, b'\r' as u16, CTRL_PRESSED)];
        assert_eq!(translate(&events), vec![b'\r']);
    }

    /// Issue #141 case 4: a plain ASCII key-down passes through via
    /// its `unicode_char`. Verifies the translator doesn't choke on
    /// non-Enter keys and that we're emitting UTF-8 of the UTF-16 char,
    /// not the virtual-key code.
    #[test]
    fn plain_ascii_key_passes_through() {
        let events = [key_down(0x41, b'a' as u16, 0)]; // VK_A = 0x41
        assert_eq!(translate(&events), vec![b'a']);
    }

    /// Issue #141 case 5: key-up events are dropped. Without this the
    /// translator would emit every Enter twice (press + release).
    #[test]
    fn key_up_emits_nothing() {
        let events = [key_up(VK_RETURN, b'\r' as u16, 0)];
        assert_eq!(translate(&events), Vec::<u8>::new());
    }

    /// Issue #141 case 6: non-key input records (mouse, focus, buffer-
    /// size, menu, window) are dropped. `ReadConsoleInputW` returns
    /// these alongside key events; the translator must skip them.
    #[test]
    fn non_key_events_emit_nothing() {
        let events = [InputEvent::NonKey, InputEvent::NonKey, InputEvent::NonKey];
        assert_eq!(translate(&events), Vec::<u8>::new());
    }

    /// Issue #141 case 7: bytes from a mixed event stream come out in
    /// event order. This pins ordering invariants — important because
    /// the production reader concatenates batches of events into one
    /// `translate` call and the child sees the result as a single
    /// write.
    #[test]
    fn mixed_event_sequence_concatenates_in_order() {
        let events = [
            key_down(0x48, b'h' as u16, 0),
            key_down(0x49, b'i' as u16, 0),
            InputEvent::NonKey, // ignored
            key_down(VK_RETURN, b'\r' as u16, SHIFT_PRESSED),
            key_up(VK_RETURN, b'\r' as u16, SHIFT_PRESSED), // ignored
            key_down(0x4D, b'm' as u16, 0),
            key_down(VK_RETURN, b'\r' as u16, 0),
        ];
        assert_eq!(translate(&events), b"hi\nm\r");
    }

    #[test]
    fn ctrl_v_uses_clipboard_image_bytes_when_available() {
        let events = [key_down(VK_V, 0x16, CTRL_PRESSED)];
        let bytes = translate_with_clipboard(&events, || Some(b"C:\\tmp\\paste.png\n".to_vec()));
        assert_eq!(bytes, b"C:\\tmp\\paste.png\n");
    }

    #[test]
    fn ctrl_v_falls_through_to_control_byte_when_clipboard_unavailable() {
        let events = [key_down(VK_V, 0x16, CTRL_PRESSED)];
        let bytes = translate_with_clipboard(&events, || None);
        assert_eq!(bytes, vec![0x16]);
    }

    // ---------- from_input_record tests ----------

    use windows::Win32::System::Console::{
        FOCUS_EVENT_RECORD, INPUT_RECORD_0, KEY_EVENT_RECORD, KEY_EVENT_RECORD_0,
        MOUSE_EVENT_RECORD, WINDOW_BUFFER_SIZE_RECORD,
    };
    use windows_core::BOOL;

    fn make_key_record(key_down_v: bool, vk: u16, ch: u16, ctrl: u32) -> INPUT_RECORD {
        INPUT_RECORD {
            EventType: KEY_EVENT as u16,
            Event: INPUT_RECORD_0 {
                KeyEvent: KEY_EVENT_RECORD {
                    bKeyDown: BOOL(if key_down_v { 1 } else { 0 }),
                    wRepeatCount: 1,
                    wVirtualKeyCode: vk,
                    wVirtualScanCode: 0,
                    uChar: KEY_EVENT_RECORD_0 { UnicodeChar: ch },
                    dwControlKeyState: ctrl,
                },
            },
        }
    }

    /// Key-event INPUT_RECORD with Shift+Enter unpacks into the
    /// right `KeyEvent` fields. This is the adapter's most important
    /// case — every layer above sees Shift via this path.
    #[test]
    fn from_input_record_decodes_shift_enter() {
        let rec = make_key_record(true, VK_RETURN, b'\r' as u16, SHIFT_PRESSED);
        let event = from_input_record(&rec).expect("some");
        match event {
            InputEvent::Key(k) => {
                assert!(k.key_down);
                assert_eq!(k.virtual_key_code, VK_RETURN);
                assert_eq!(k.control_key_state & SHIFT_PRESSED, SHIFT_PRESSED);
            }
            InputEvent::NonKey => panic!("expected key event"),
        }
        // Sanity: the round-trip through translate produces \n.
        assert_eq!(translate(&[event]), vec![b'\n']);
    }

    /// Key-event INPUT_RECORD with plain Enter unpacks with no shift.
    /// Pair to the test above so regressions on either side surface.
    #[test]
    fn from_input_record_decodes_plain_enter() {
        let rec = make_key_record(true, VK_RETURN, b'\r' as u16, 0);
        let event = from_input_record(&rec).expect("some");
        assert_eq!(translate(&[event]), vec![b'\r']);
    }

    /// Mouse INPUT_RECORD becomes `NonKey`. Same for focus,
    /// buffer-size, and menu records — collapsing them lets the
    /// caller observe activity without caring about the variant.
    #[test]
    fn from_input_record_collapses_non_key_variants() {
        let mouse = INPUT_RECORD {
            EventType: MOUSE_EVENT as u16,
            Event: INPUT_RECORD_0 {
                MouseEvent: MOUSE_EVENT_RECORD::default(),
            },
        };
        let focus = INPUT_RECORD {
            EventType: FOCUS_EVENT as u16,
            Event: INPUT_RECORD_0 {
                FocusEvent: FOCUS_EVENT_RECORD::default(),
            },
        };
        let bufsize = INPUT_RECORD {
            EventType: WINDOW_BUFFER_SIZE_EVENT as u16,
            Event: INPUT_RECORD_0 {
                WindowBufferSizeEvent: WINDOW_BUFFER_SIZE_RECORD::default(),
            },
        };
        for rec in [mouse, focus, bufsize] {
            assert_eq!(from_input_record(&rec), Some(InputEvent::NonKey));
        }
    }

    /// Unknown EventType yields None. Defensive: future Windows
    /// versions could add record types; the adapter shouldn't
    /// silently classify them as NonKey (which would block downstream
    /// detection logic).
    #[test]
    fn from_input_record_returns_none_for_unknown_event_type() {
        let rec = INPUT_RECORD {
            EventType: 0xFFFF,
            Event: INPUT_RECORD_0 {
                MouseEvent: MOUSE_EVENT_RECORD::default(),
            },
        };
        assert_eq!(from_input_record(&rec), None);
    }

    // ---------- spawn_with_source tests ----------

    use std::sync::atomic::{AtomicUsize, Ordering as StdOrdering};
    use std::sync::Arc as StdArc;
    use std::time::Duration;

    /// The worker translates events from `read_events` and forwards
    /// the bytes on the channel. Feeding a Shift+Enter event must
    /// surface as `\n` on the receiver.
    #[test]
    fn spawn_with_source_emits_translated_bytes() {
        let events_emitted = StdArc::new(AtomicUsize::new(0));
        let emitted = StdArc::clone(&events_emitted);
        let mut handle = spawn_with_source(move || {
            // Emit the canned events once, then return empty forever
            // so the worker spins (cancelled by Drop).
            if emitted.fetch_add(1, StdOrdering::SeqCst) == 0 {
                vec![
                    InputEvent::Key(KeyEvent {
                        key_down: true,
                        virtual_key_code: VK_RETURN,
                        unicode_char: b'\r' as u16,
                        control_key_state: SHIFT_PRESSED,
                    }),
                    InputEvent::Key(KeyEvent {
                        key_down: true,
                        virtual_key_code: VK_RETURN,
                        unicode_char: b'\r' as u16,
                        control_key_state: 0,
                    }),
                ]
            } else {
                // Avoid 100% CPU in the test's "no events" period.
                std::thread::sleep(Duration::from_millis(5));
                Vec::new()
            }
        });
        let rx = handle.take_receiver().expect("take_receiver");
        let chunk = rx.recv_timeout(Duration::from_secs(2)).expect("chunk");
        assert_eq!(chunk, b"\n\r");
        // Drop the handle — worker should observe cancel and exit.
        drop(handle);
    }

    /// When the source emits non-key events only, translate returns
    /// nothing and the worker MUST NOT send empty chunks. Verifies the
    /// empty-bytes guard in `spawn_with_source`.
    #[test]
    fn spawn_with_source_skips_empty_output() {
        let count = StdArc::new(AtomicUsize::new(0));
        let c = StdArc::clone(&count);
        let mut handle = spawn_with_source(move || {
            if c.fetch_add(1, StdOrdering::SeqCst) < 3 {
                vec![InputEvent::NonKey]
            } else {
                std::thread::sleep(Duration::from_millis(5));
                Vec::new()
            }
        });
        let rx = handle.take_receiver().expect("take_receiver");
        // No chunks should ever arrive — the receiver times out.
        let result = rx.recv_timeout(Duration::from_millis(200));
        assert!(result.is_err(), "received unexpected chunk: {:?}", result);
        drop(handle);
    }

    /// Dropping the handle signals cancel and joins the worker.
    /// Without this the worker thread would outlive the test process
    /// in some cases (and definitely outlive the PTY session in
    /// production).
    #[test]
    fn drop_cancels_worker() {
        let count = StdArc::new(AtomicUsize::new(0));
        let c = StdArc::clone(&count);
        let handle = spawn_with_source(move || {
            c.fetch_add(1, StdOrdering::SeqCst);
            std::thread::sleep(Duration::from_millis(5));
            Vec::new()
        });
        // Let the worker loop a few times.
        std::thread::sleep(Duration::from_millis(40));
        let observed_before_drop = count.load(StdOrdering::SeqCst);
        drop(handle);
        // After drop, the count should stop advancing (worker exited).
        // Give it a little time to fully exit.
        std::thread::sleep(Duration::from_millis(50));
        let observed_after_drop = count.load(StdOrdering::SeqCst);
        // Worst case the worker did 1-2 more iterations during the
        // cancel-check window (10ms sleep + cancel check). Allow up to
        // 5 extra to keep the test stable on slow CI runners.
        assert!(
            observed_after_drop - observed_before_drop <= 5,
            "worker kept iterating after drop: {observed_before_drop} -> {observed_after_drop}"
        );
    }
}
