//! Terminal modes a child TUI leaves on, and the bytes that turn them off.
//!
//! Ctrl+C ends a session by killing the child (`interrupt_pty_process`), so
//! a TUI such as Claude Code never runs its own exit path: it can leave the
//! user's terminal on the alternate screen, with a scroll region, application
//! cursor keys, colours, a hidden cursor, or mouse, focus and paste reporting
//! on. [`ChildModes`] watches the child's output on its way to the terminal
//! and knows which of those are still on; [`ChildModes::take_reset`] returns
//! the bytes that undo exactly those, so a mode the child never touched is
//! never touched on its behalf. `?1049l` on a terminal that was never on the
//! alternate screen, for one, would restore a stale saved cursor.

use std::collections::BTreeSet;

use vte::{Params, Parser, Perform};

/// The modes a child has on, as of the output observed so far.
#[derive(Debug, Default)]
struct ModeState {
    /// The alternate-screen mode in use (`47`, `1047` or `1049`).
    alt_screen: Option<u16>,
    /// DECSTBM set to less than the full screen.
    scroll_region: bool,
    /// DECCKM: arrow keys send `ESC O A` instead of `ESC [ A`.
    cursor_keys_app: bool,
    /// DECKPAM: the keypad sends application sequences.
    keypad_app: bool,
    /// DECAWM turned off.
    autowrap_off: bool,
    /// DECTCEM turned off.
    cursor_hidden: bool,
    /// Mouse reporting and encoding modes that are on.
    mouse: BTreeSet<u16>,
    /// `1004` focus reporting.
    focus: bool,
    /// `2004` bracketed paste.
    paste: bool,
    /// An SGR attribute is set (colour, bold, …) that `SGR 0` has not cleared.
    sgr: bool,
}

/// Mouse reporting (`1000`–`1003`) and encodings (`1005`, `1006`, `1015`,
/// `1016`).
const MOUSE_MODES: [u16; 8] = [1000, 1001, 1002, 1003, 1005, 1006, 1015, 1016];

impl Perform for ModeState {
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        let private = intermediates == b"?";
        let values = || params.iter().filter_map(|p| p.first().copied());
        match action {
            'h' | 'l' if private => {
                let on = action == 'h';
                for mode in values() {
                    self.private_mode(mode, on);
                }
            }
            // DECSTBM. No full `top;bottom` pair, or one starting at line 1
            // with no bottom, is the reset to the full screen.
            'r' if intermediates.is_empty() => {
                let mut it = values();
                self.scroll_region = matches!(
                    (it.next(), it.next()),
                    (Some(top), Some(bottom)) if bottom > top
                );
            }
            'm' if intermediates.is_empty() => self.sgr = sgr_left_set(self.sgr, values()),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if !intermediates.is_empty() {
            return;
        }
        match byte {
            b'=' => self.keypad_app = true,
            b'>' => self.keypad_app = false,
            // RIS: a full reset leaves nothing on.
            b'c' => *self = Self::default(),
            _ => {}
        }
    }
}

/// Whether an attribute is still set after one SGR sequence. `0` clears
/// everything; any other attribute sets one. The arguments of an extended
/// colour (`38`/`48`/`58` then `5;n` or `2;r;g;b`) are skipped, so colour
/// index `0` is not mistaken for a reset. A bare `CSI m` arrives as one `0`.
fn sgr_left_set(mut set: bool, params: impl Iterator<Item = u16>) -> bool {
    let mut params = params;
    while let Some(value) = params.next() {
        match value {
            0 => set = false,
            38 | 48 | 58 => {
                set = true;
                let skip = match params.next() {
                    Some(5) => 1,
                    Some(2) => 3,
                    _ => 0,
                };
                for _ in 0..skip {
                    params.next();
                }
            }
            _ => set = true,
        }
    }
    set
}

impl ModeState {
    fn private_mode(&mut self, mode: u16, on: bool) {
        match mode {
            1 => self.cursor_keys_app = on,
            7 => self.autowrap_off = !on,
            25 => self.cursor_hidden = !on,
            47 | 1047 | 1049 => {
                self.alt_screen = if on {
                    Some(mode)
                } else {
                    self.alt_screen.filter(|&current| current != mode)
                }
            }
            1004 => self.focus = on,
            2004 => self.paste = on,
            _ if MOUSE_MODES.contains(&mode) => {
                if on {
                    self.mouse.insert(mode);
                } else {
                    self.mouse.remove(&mode);
                }
            }
            _ => {}
        }
    }

    /// The bytes that turn off everything still on, in a safe order: leave
    /// the alternate screen first, so the rest applies to the main screen the
    /// shell is on.
    fn reset(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(mode) = self.alt_screen {
            out.extend_from_slice(format!("\x1b[?{mode}l").as_bytes());
        }
        if self.scroll_region {
            // DECSTBM homes the cursor: save and restore it around the reset.
            out.extend_from_slice(b"\x1b7\x1b[r\x1b8");
        }
        if self.cursor_keys_app {
            out.extend_from_slice(b"\x1b[?1l");
        }
        if self.keypad_app {
            out.extend_from_slice(b"\x1b>");
        }
        if self.autowrap_off {
            out.extend_from_slice(b"\x1b[?7h");
        }
        for mode in &self.mouse {
            out.extend_from_slice(format!("\x1b[?{mode}l").as_bytes());
        }
        if self.focus {
            out.extend_from_slice(b"\x1b[?1004l");
        }
        if self.paste {
            out.extend_from_slice(b"\x1b[?2004l");
        }
        if self.sgr {
            out.extend_from_slice(b"\x1b[0m");
        }
        if self.cursor_hidden {
            out.extend_from_slice(b"\x1b[?25h");
        }
        out
    }
}

/// Follows a child's output stream; one per session.
#[derive(Default)]
pub(crate) struct ChildModes {
    /// Carries a sequence split across two reads.
    parser: Parser,
    state: ModeState,
}

impl std::fmt::Debug for ChildModes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildModes")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl ChildModes {
    /// Observe child output, before it is written to the terminal.
    pub(crate) fn observe(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.state, bytes);
    }

    /// The bytes that turn off what the child left on, once. A second call
    /// returns nothing until the child turns something on again.
    pub(crate) fn take_reset(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.state).reset()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset_after(output: &[u8]) -> Vec<u8> {
        let mut modes = ChildModes::default();
        modes.observe(output);
        modes.take_reset()
    }

    #[test]
    fn a_child_that_left_nothing_on_gets_no_reset() {
        assert!(reset_after(b"plain text\r\n").is_empty());
        // Turned on, then off again by the child itself.
        assert!(reset_after(
            b"\x1b[?1049h\x1b[?1h\x1b=\x1b[?1003;1006h\x1b[?25l\x1b[1;31m\x1b[2;10r\
              \x1b[r\x1b[0m\x1b[?25h\x1b[?1003;1006l\x1b>\x1b[?1l\x1b[?1049l"
        )
        .is_empty());
    }

    #[test]
    fn everything_a_killed_tui_left_on_is_turned_off_alt_screen_first() {
        let reset = reset_after(
            b"\x1b[?1049h\x1b[2;20r\x1b[?1h\x1b=\x1b[?7l\x1b[?1003h\x1b[?1006h\
              \x1b[?1004h\x1b[?2004h\x1b[38;5;208m\x1b[?25l",
        );
        assert_eq!(
            reset,
            b"\x1b[?1049l\x1b7\x1b[r\x1b8\x1b[?1l\x1b>\x1b[?7h\x1b[?1003l\x1b[?1006l\
              \x1b[?1004l\x1b[?2004l\x1b[0m\x1b[?25h"
                .to_vec()
        );
    }

    #[test]
    fn only_the_alternate_screen_mode_the_child_used_is_left() {
        assert_eq!(reset_after(b"\x1b[?47h"), b"\x1b[?47l");
        assert_eq!(reset_after(b"\x1b[?1047h"), b"\x1b[?1047l");
        assert!(reset_after(b"\x1b[?1049h\x1b[?1049l").is_empty());
    }

    #[test]
    fn sequences_split_across_reads_are_followed() {
        let stream = b"\x1b[?1049h\x1b[?2004h\x1b[1m";
        for split in 1..stream.len() {
            let mut modes = ChildModes::default();
            modes.observe(&stream[..split]);
            modes.observe(&stream[split..]);
            assert_eq!(
                modes.take_reset(),
                b"\x1b[?1049l\x1b[?2004l\x1b[0m".to_vec(),
                "split at {split}"
            );
        }
    }

    #[test]
    fn a_full_scroll_region_reset_and_sgr_reset_count_as_off() {
        assert!(reset_after(b"\x1b[3;9r\x1b[r").is_empty());
        assert!(reset_after(b"\x1b[3;9r\x1b[;r").is_empty());
        assert!(reset_after(b"\x1b[31m\x1b[m").is_empty());
        assert!(reset_after(b"\x1b[31m\x1b[0m").is_empty());
        assert_eq!(reset_after(b"\x1b[0;31m"), b"\x1b[0m");
        assert!(reset_after(b"\x1b[31;0m").is_empty(), "a trailing 0 resets");
        assert_eq!(
            reset_after(b"\x1b[38;5;0m"),
            b"\x1b[0m",
            "colour index 0 is not a reset"
        );
        assert_eq!(reset_after(b"\x1b[48;2;0;0;0m"), b"\x1b[0m");
    }

    #[test]
    fn ris_and_unrelated_sequences() {
        assert!(reset_after(b"\x1b[?1049h\x1b[?25l\x1bc").is_empty());
        assert!(reset_after(b"\x1b[2J\x1b[H\x1b[5n\x1b[?9001h\x1b]0;t\x07").is_empty());
    }

    #[test]
    fn the_reset_is_taken_once() {
        let mut modes = ChildModes::default();
        modes.observe(b"\x1b[?1004h");
        assert_eq!(modes.take_reset(), b"\x1b[?1004l");
        assert!(modes.take_reset().is_empty());
        modes.observe(b"\x1b[?25l");
        assert_eq!(modes.take_reset(), b"\x1b[?25h");
    }
}
