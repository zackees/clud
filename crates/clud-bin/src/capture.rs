//! Server-side terminal emulator state for detachable PTY sessions.
//!
//! The daemon worker feeds every PTY output chunk into a `TerminalCapture`.
//! When a client (local `clud attach`, or a future xterm.js frontend) connects,
//! the capture emits a **synthesized repaint** — an ANSI byte stream that, when
//! played into a freshly-initialized terminal, reproduces the exact screen the
//! backend (codex / claude) is currently rendering: cells, SGR attributes,
//! cursor position, alt-screen state, window title, and mode flags.
//!
//! Without this layer, a mid-session attach sees raw bytes since launch —
//! fine for line-oriented CLIs but wrong for TUIs that use cursor moves,
//! partial redraws, and the alternate screen buffer. See issue #34.

use vt100::Parser;

/// Scrollback kept inside the parser. The attach snapshot only replays the
/// visible screen, so this buffer is for future features (e.g. history-aware
/// clients); keeping it small bounds memory.
const SCROLLBACK_LINES: usize = 1024;

pub struct TerminalCapture {
    parser: Parser,
    rows: u16,
    cols: u16,
}

impl TerminalCapture {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: Parser::new(rows, cols, SCROLLBACK_LINES),
            rows,
            cols,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.parser.screen_mut().set_size(rows, cols);
        self.rows = rows;
        self.cols = cols;
    }

    #[cfg(test)]
    pub fn size(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }

    /// Build an ANSI byte stream that, when fed to a fresh terminal emulator
    /// of size `(rows, cols)`, reproduces the current display state: cells,
    /// SGR, cursor, alt-screen, title, mouse/bracketed-paste modes.
    ///
    /// `vt100::Screen::contents_formatted` already emits cells + SGR + cursor
    /// positioning as a replay stream for the active screen. We prepend a
    /// terminal reset so the client starts from a clean slate, and re-assert
    /// alt-screen + title since those are "sticky" modes a fresh terminal
    /// won't have set.
    pub fn snapshot_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4096);
        // RIS — full reset. Clears SGR, modes, scroll region, cursor style,
        // and the primary screen. Alt-screen is handled explicitly below.
        out.extend_from_slice(b"\x1bc");

        let screen = self.parser.screen();

        // If the app is on the alternate screen, enter it *before* painting
        // cells so the paint lands on the alt buffer and the primary buffer
        // stays empty (matching the source terminal). `contents_formatted`
        // paints only the active grid and does not emit this mode switch
        // itself.
        if screen.alternate_screen() {
            out.extend_from_slice(b"\x1b[?1049h");
        }

        // `state_formatted` = cells + SGR + cursor position/visibility +
        // input mode (bracketed paste, app cursor keys, app keypad, mouse
        // protocol). Emits absolute cursor moves, so the client doesn't need
        // a separate repositioning step.
        out.extend(screen.state_formatted());

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a scripted sequence, snapshot, feed the snapshot into a fresh
    /// parser, and assert the two screens render identically. This is the
    /// core guarantee for mid-session attach.
    fn assert_replay_equivalent(rows: u16, cols: u16, script: &[u8]) {
        let mut source = TerminalCapture::new(rows, cols);
        source.feed(script);
        let snapshot = source.snapshot_bytes();

        let mut replay = Parser::new(rows, cols, SCROLLBACK_LINES);
        replay.process(&snapshot);

        let src = source.parser.screen();
        let dst = replay.screen();

        assert_eq!(
            src.contents(),
            dst.contents(),
            "cell contents diverge after replay"
        );
        assert_eq!(
            src.cursor_position(),
            dst.cursor_position(),
            "cursor position diverges after replay"
        );
        assert_eq!(
            src.alternate_screen(),
            dst.alternate_screen(),
            "alt-screen flag diverges after replay"
        );
        assert_eq!(
            src.hide_cursor(),
            dst.hide_cursor(),
            "cursor visibility diverges after replay"
        );
    }

    #[test]
    fn replay_round_trip_plain_text() {
        assert_replay_equivalent(24, 80, b"hello world");
    }

    #[test]
    fn replay_round_trip_sgr_and_cursor() {
        // Red "ERR" at row 3, col 5, then reset + green "ok" somewhere else.
        let script = b"\x1b[3;5H\x1b[31mERR\x1b[0m\x1b[10;1H\x1b[32mok\x1b[0m";
        assert_replay_equivalent(24, 80, script);
    }

    #[test]
    fn replay_round_trip_alt_screen() {
        // Enter alt screen, paint something, stay on alt screen.
        let script = b"\x1b[?1049h\x1b[2J\x1b[1;1Hhello-alt";
        assert_replay_equivalent(24, 80, script);
    }

    #[test]
    fn replay_round_trip_hidden_cursor() {
        let script = b"\x1b[?25lhidden";
        assert_replay_equivalent(24, 80, script);
    }

    #[test]
    fn resize_updates_parser_dims() {
        let mut cap = TerminalCapture::new(24, 80);
        cap.resize(40, 120);
        assert_eq!(cap.size(), (40, 120));
        // Noop resize should not panic or change anything.
        cap.resize(40, 120);
        assert_eq!(cap.size(), (40, 120));
    }

    /// Feeding the parser in multiple small chunks (as the daemon does, one
    /// per PTY read) must produce the same final state as one big feed. This
    /// is the invariant that lets `push_output` call `capture.feed` on every
    /// chunk without needing to buffer.
    #[test]
    fn chunked_feed_matches_single_feed() {
        let script: &[u8] = b"\x1b[1;1Hhello \x1b[31mworld\x1b[0m\x1b[5;10H!";
        let mut whole = TerminalCapture::new(24, 80);
        whole.feed(script);
        let mut chunked = TerminalCapture::new(24, 80);
        for byte in script {
            chunked.feed(std::slice::from_ref(byte));
        }
        assert_eq!(
            whole.parser.screen().contents(),
            chunked.parser.screen().contents()
        );
        assert_eq!(whole.snapshot_bytes(), chunked.snapshot_bytes());
    }

    /// A mid-session attach scenario: a TUI does several partial redraws
    /// (clear line, rewrite). A naive raw-byte replay would layer all three
    /// stages and look wrong; the grid-based snapshot shows only the final
    /// frame, and a fresh parser fed the snapshot ends up at the same state.
    #[test]
    fn mid_session_attach_reproduces_final_frame() {
        let mut source = TerminalCapture::new(24, 80);
        // Frame 1: three lines
        source.feed(b"\x1b[1;1HLine1\x1b[2;1HLine2\x1b[3;1HLine3");
        // Frame 2: clear line 2 and rewrite
        source.feed(b"\x1b[2;1H\x1b[K\x1b[2;1Hreplaced");
        // Frame 3: add a status line
        source.feed(b"\x1b[24;1H\x1b[7mstatus\x1b[0m");

        let snapshot = source.snapshot_bytes();

        let mut replay = Parser::new(24, 80, SCROLLBACK_LINES);
        replay.process(&snapshot);

        let src = source.parser.screen();
        let dst = replay.screen();
        // The final frame has "Line1", "replaced" (not "Line2"), "Line3",
        // blank rows, and "status" in inverse on row 24.
        assert_eq!(src.contents(), dst.contents());
        assert!(dst.contents().contains("Line1"));
        assert!(dst.contents().contains("replaced"));
        assert!(!dst.contents().contains("Line2"));
        assert!(dst.contents().contains("status"));
    }

    #[test]
    fn replay_round_trip_utf8_multibyte() {
        // Box-drawing + emoji. A raw-byte replay would be fine here, but the
        // grid path has to keep UTF-8 sequences intact across parser state.
        let script = "\x1b[1;1H┌─┐\n│ai│\n└─┘\x1b[5;1H🚀 launch".as_bytes();
        assert_replay_equivalent(24, 80, script);
    }

    #[test]
    fn replay_round_trip_extended_sgr() {
        // Bold, italic, underline, inverse, and a 256-color background.
        let script = b"\x1b[1;1H\
            \x1b[1mBOLD\x1b[0m \
            \x1b[3mitalic\x1b[0m \
            \x1b[4munder\x1b[0m \
            \x1b[7minv\x1b[0m \
            \x1b[48;5;27mbg256\x1b[0m\
            \x1b[2;1H\x1b[38;2;255;128;0mtruecolor\x1b[0m";
        assert_replay_equivalent(24, 80, script);
    }

    #[test]
    fn replay_round_trip_scroll_region() {
        // DECSTBM: set scroll region to rows 2-5, write, scroll.
        let script = b"\x1b[2;5r\x1b[2;1Ha\n\x1b[3;1Hb\n\x1b[4;1Hc\n\x1b[5;1Hd\n";
        assert_replay_equivalent(24, 80, script);
    }

    #[test]
    fn replay_round_trip_byte_by_byte_feed() {
        // Pathological case: one byte at a time, including through the middle
        // of multi-byte UTF-8 and CSI sequences. The parser is a streaming
        // VTE, so this must be equivalent to a single feed.
        let script = "\x1b[31m重要: \x1b[1;4mheads\x1b[0m ok".as_bytes();
        let mut whole = TerminalCapture::new(24, 80);
        whole.feed(script);
        let mut drip = TerminalCapture::new(24, 80);
        for b in script {
            drip.feed(std::slice::from_ref(b));
        }
        assert_eq!(whole.snapshot_bytes(), drip.snapshot_bytes());
    }

    #[test]
    fn replay_round_trip_partial_clear_and_redraw() {
        // Simulate a TUI doing a partial redraw: fill, clear a region,
        // repaint part of it. A raw-byte replay would show all three stages
        // layered; the grid-based replay shows only the final frame.
        let script = b"\
            \x1b[1;1HLine1\
            \x1b[2;1HLine2\
            \x1b[3;1HLine3\
            \x1b[2;1H\x1b[K\
            \x1b[2;1Hreplaced\
        ";
        assert_replay_equivalent(24, 80, script);
    }
}
