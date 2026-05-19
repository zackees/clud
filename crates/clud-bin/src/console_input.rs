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

#![cfg(windows)]

/// Windows virtual-key code for the Enter / Return key.
/// Mirrors `winuser.h`'s `VK_RETURN` so the test module doesn't need a
/// link-time dependency on `windows-sys`.
pub const VK_RETURN: u16 = 0x0D;

/// `KEY_EVENT_RECORD::dwControlKeyState` bit for Left- or Right-Shift.
/// Mirrors `wincon.h`'s `SHIFT_PRESSED`.
pub const SHIFT_PRESSED: u32 = 0x0010;

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
}
