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
use vte::{Params, Parser as VteParser, Perform};

/// Scrollback kept inside the parser. The attach snapshot only replays the
/// visible screen, so this buffer is for future features (e.g. history-aware
/// clients); keeping it small bounds memory.
const SCROLLBACK_LINES: usize = 1024;

/// Upper bound on a persisted window title, in bytes. Titles come from the
/// child process, which is untrusted input: vte buffers an OSC payload
/// without a length limit, so an unbounded title would inflate every
/// snapshot and every reattach payload. 256 bytes is longer than any real
/// title and keeps `snapshot_bytes` bounded.
const MAX_TITLE_BYTES: usize = 256;

pub struct TerminalCapture {
    parser: Parser,
    rows: u16,
    cols: u16,
    /// Sticky modes vt100 tracks internally but does not expose / round-trip
    /// through `state_formatted`, plus the window title, which vt100 never
    /// tracks at all. Sniffed from the byte stream in parallel with vt100's
    /// own parse, then re-emitted in `snapshot_bytes`. See #36.
    sticky: StickyModes,
    /// Separate vte::Parser driving `sticky` — vt100 already uses vte for
    /// its own state, this one only watches the handful of sequences we
    /// need to restore after RIS.
    sniffer: VteParser,
}

/// Modes vt100 0.16 doesn't round-trip through `state_formatted`.
/// The window title is different: vt100 does not track it at all, so it is
/// recorded here as well.
#[derive(Default)]
struct StickyModes {
    /// DECSTBM (`\x1b[<top>;<bot>r`), 1-indexed. `None` = full screen.
    decstbm: Option<(u16, u16)>,
    /// DECAWM off flag (`\x1b[?7l`). `false` = default (autowrap on).
    decawm_off: bool,
    /// Latest OSC 0 / OSC 2 window title, already sanitized (control
    /// characters and undecodable bytes dropped, truncated to
    /// `MAX_TITLE_BYTES`). `None` = the app
    /// never set one, so the snapshot leaves the client's title alone.
    /// `Some("")` = the app explicitly cleared it, which we replay.
    title: Option<String>,
}

/// Sanitize an untrusted OSC title payload for later re-emission.
///
/// `snapshot_bytes` replays these bytes verbatim into the attaching client's
/// terminal, so nothing that could end the OSC string early (BEL) or start a
/// fresh escape sequence (ESC, CSI as a raw C1 byte) may survive. Every
/// control character is dropped rather than escaped: `char::is_control`
/// covers C0 (U+0000..=U+001F), DEL (U+007F) and C1 (U+0080..=U+009F).
/// Invalid UTF-8 becomes U+FFFD through lossy decoding and is dropped as
/// well — a byte the child could not encode has no business becoming a
/// glyph in the user's title bar. The result is truncated on a char
/// boundary so the output is always valid UTF-8 and at most
/// `MAX_TITLE_BYTES` long.
fn sanitize_title(raw: &[u8]) -> String {
    let mut out = String::new();
    for ch in String::from_utf8_lossy(raw).chars() {
        if ch.is_control() || ch == char::REPLACEMENT_CHARACTER {
            continue;
        }
        if out.len() + ch.len_utf8() > MAX_TITLE_BYTES {
            break;
        }
        out.push(ch);
    }
    out
}

struct StickySniffer<'a> {
    modes: &'a mut StickyModes,
}

impl<'a> Perform for StickySniffer<'a> {
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        let private = intermediates.first() == Some(&b'?');
        match action {
            // DECSTBM (`\x1b[<top>;<bot>r`). vte always pushes at least one
            // param on CSI dispatch (even an empty param becomes `0`), so
            // `is_empty()` alone doesn't reliably detect "reset to full
            // screen". We treat "no valid top+bottom pair" as reset, which
            // matches ECMA-48 semantics.
            'r' if !private => {
                let mut iter = params.iter();
                let top = iter.next().and_then(|p| p.first().copied());
                let bot = iter.next().and_then(|p| p.first().copied());
                match (top, bot) {
                    (Some(t), Some(b)) if t >= 1 && b > t => {
                        self.modes.decstbm = Some((t, b));
                    }
                    _ => {
                        self.modes.decstbm = None;
                    }
                }
            }
            // Private-mode set / reset.
            'h' | 'l' => {
                if !private {
                    return;
                }
                let on = action == 'h';
                for p in params.iter() {
                    if let Some(&pv) = p.first() {
                        if pv == 7 {
                            self.modes.decawm_off = !on;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        // OSC 0 = icon name + window title, OSC 2 = window title. OSC 1
        // (icon name only) and every other OSC (8 hyperlink, 10/11 colors,
        // 52 clipboard) is not a window title, so it is ignored.
        let Some(&kind) = params.first() else {
            return;
        };
        if kind != b"0" && kind != b"2" {
            return;
        }
        // vte splits the payload on `;`, but a title may legitimately
        // contain one, so rejoin everything after the numeric kind. vte caps
        // an OSC at 16 params, so a title with 15+ semicolons loses its tail
        // — an acceptable loss for a cosmetic string.
        let raw: Vec<u8> = params[1..].join(&b';');
        self.modes.title = Some(sanitize_title(&raw));
    }
}

impl TerminalCapture {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: Parser::new(rows, cols, SCROLLBACK_LINES),
            rows,
            cols,
            sticky: StickyModes::default(),
            sniffer: VteParser::new(),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        let mut sniffer = StickySniffer {
            modes: &mut self.sticky,
        };
        self.sniffer.advance(&mut sniffer, bytes);
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

        // Re-assert sticky modes that RIS clears and `state_formatted`
        // doesn't re-emit. vt100 0.16 doesn't expose these publicly, so we
        // sniff them from the byte stream in `feed`. Issue #36.
        if let Some((top, bot)) = self.sticky.decstbm {
            // DECSTBM (scroll region). Without this a TUI with a narrow
            // scroll region would lose it on reattach and subsequent scrolls
            // would scroll the whole screen.
            out.extend_from_slice(format!("\x1b[{};{}r", top, bot).as_bytes());
        }
        if self.sticky.decawm_off {
            // DECAWM off. Without this, a TUI that had disabled autowrap
            // (e.g. a drawing app using absolute moves) would start wrapping
            // text after reattach — garbled layout.
            out.extend_from_slice(b"\x1b[?7l");
        }

        // Re-assert the window title. vt100 does not track it at all, so the
        // sniffer records OSC 0/2 in `feed` and we replay it here, after the
        // RIS that clears it on a real terminal. Always emitted as OSC 0
        // (icon + title), BEL-terminated — the form every terminal clud
        // targets accepts. The payload was sanitized on the way in, so it
        // cannot close the sequence early or inject another escape.
        if let Some(title) = self.sticky.title.as_deref() {
            out.extend_from_slice(b"\x1b]0;");
            out.extend_from_slice(title.as_bytes());
            out.push(0x07);
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

#[cfg(test)]
mod adversarial_tests {
    //! Tests that actively try to break the replay synthesis — scroll
    //! regions, DECSC/DECRC, alt-screen toggles with state, wide/zero-width
    //! glyphs, wrap-at-margin SGR continuity, malformed input, resize mid-
    //! alt-screen. Each test either locks in correct behavior or documents a
    //! known limitation with an inline note linking to issue #36.
    //!
    //! The helper `adv_replay` differs from `assert_replay_equivalent` above:
    //! it returns the two screens so individual tests can assert only the
    //! properties they care about (cell content, cursor position, specific
    //! cells) rather than full equivalence. Some terminal state that vt100
    //! doesn't track (the OSC-4 palette, the saved-cursor register) will
    //! never survive a round-trip; the tests for those cases assert the
    //! limit, not the wish. The window title used to be in that list —
    //! `StickyModes::title` sniffs it out of the byte stream instead, so it
    //! now round-trips (#1205).
    use super::*;
    use vt100::Parser;

    /// Feed `script` into a capture, take the snapshot, feed it into a fresh
    /// parser. Return the (source, replay) parsers so callers can pick their
    /// assertions.
    fn adv_replay(rows: u16, cols: u16, script: &[u8]) -> (Parser, Parser) {
        let mut source = TerminalCapture::new(rows, cols);
        source.feed(script);
        let snapshot = source.snapshot_bytes();

        // Move parser out of TerminalCapture for inspection. The simplest way
        // is to reconstruct a fresh parser and re-feed the original script
        // — vt100 doesn't expose Parser by value, only by reference.
        let mut src_parser = Parser::new(rows, cols, SCROLLBACK_LINES);
        src_parser.process(script);

        let mut replay = Parser::new(rows, cols, SCROLLBACK_LINES);
        replay.process(&snapshot);

        (src_parser, replay)
    }

    // ─── Scroll region (DECSTBM) ────────────────────────────────────────────

    #[test]
    fn scroll_region_is_reflected_in_final_contents() {
        // Set scroll region to rows 3-20 (1-indexed), then paint several
        // lines. The source and replay should show the same final frame even
        // though the scroll region affects where newlines wrap.
        let script = b"\x1b[3;20r\x1b[3;1Ha\x1b[4;1Hb\x1b[5;1Hc";
        let (src, dst) = adv_replay(24, 80, script);
        assert_eq!(src.screen().contents(), dst.screen().contents());
        assert_eq!(
            src.screen().cursor_position(),
            dst.screen().cursor_position()
        );
    }

    // ─── Save / restore cursor (DECSC / DECRC) ──────────────────────────────

    #[test]
    fn saved_cursor_not_preserved_by_snapshot_known_limitation() {
        // Source uses \x1b7 to save cursor at row 5, paints elsewhere, then
        // \x1b8 to restore. By the time we snapshot, cursor IS back at row 5
        // and the grid reflects a correct final position. What we CANNOT
        // preserve is the "saved-cursor register" itself: if the app issues
        // another \x1b8 AFTER the reattach, our replay doesn't carry the
        // saved position forward.
        //
        // This test documents the limit: post-snapshot \x1b8 in the source
        // still returns to (5, 5), but in the replayed parser it returns to
        // (0, 0) because we never re-saved. Filed under #36.
        let script = b"\x1b[5;5H\x1b7\x1b[20;20Htail";
        let (mut src, mut dst) = adv_replay(24, 80, script);
        src.process(b"\x1b8");
        dst.process(b"\x1b8");
        assert_eq!(
            src.screen().cursor_position(),
            (4, 4),
            "source DECRC returns to saved"
        );
        assert_ne!(
            dst.screen().cursor_position(),
            (4, 4),
            "replay DECRC loses the saved cursor — known limitation"
        );
    }

    // ─── Alt-screen toggle with state on both buffers ───────────────────────

    #[test]
    fn alt_screen_round_trip_when_active() {
        // Enter alt, paint, stay on alt. Primary is irrelevant.
        let script = b"\x1b[?1049h\x1b[2J\x1b[1;1HALT_VIEW";
        let (src, dst) = adv_replay(24, 80, script);
        assert!(src.screen().alternate_screen());
        assert!(dst.screen().alternate_screen());
        assert!(dst.screen().contents().contains("ALT_VIEW"));
    }

    #[test]
    fn primary_content_visible_after_leaving_alt() {
        // Paint primary, enter alt + paint, leave alt — primary content must
        // be what's shown in the replay.
        let script = b"\x1b[1;1HPRIMARY\x1b[?1049h\x1b[2J\x1b[1;1HALT\x1b[?1049l";
        let (src, dst) = adv_replay(24, 80, script);
        assert!(!src.screen().alternate_screen());
        assert!(!dst.screen().alternate_screen());
        assert!(
            dst.screen().contents().contains("PRIMARY"),
            "PRIMARY missing from replay: {:?}",
            dst.screen().contents()
        );
        // ALT content should NOT bleed through.
        assert!(!dst.screen().contents().contains("ALT"));
    }

    // ─── SGR across wrapped rows ────────────────────────────────────────────

    #[test]
    fn sgr_preserved_across_line_wrap() {
        // Narrow grid so text wraps. Assert SGR color lands on the right cell
        // on the wrapped row — vt100's contents_formatted must re-emit SGR
        // on each row where it's active.
        let script = b"\x1b[31mAAAAAAAAAA"; // 10 red As on a 5-col grid → wraps
        let (src, dst) = adv_replay(4, 5, script);
        // Assert same red-vs-default pattern on both parsers.
        for row in 0..2u16 {
            for col in 0..5u16 {
                let sc = src.screen().cell(row, col).expect("src cell");
                let dc = dst.screen().cell(row, col).expect("dst cell");
                assert_eq!(
                    sc.fgcolor(),
                    dc.fgcolor(),
                    "fgcolor differs at ({},{})",
                    row,
                    col
                );
                assert_eq!(sc.contents(), dc.contents());
            }
        }
    }

    // ─── Wide / CJK glyphs ──────────────────────────────────────────────────

    #[test]
    fn wide_glyphs_land_at_same_cells() {
        // CJK chars take 2 columns. Place them near the right margin and
        // verify cell-by-cell equivalence.
        let script = "\x1b[1;1H日本語test".as_bytes();
        let (src, dst) = adv_replay(24, 80, script);
        assert_eq!(src.screen().contents(), dst.screen().contents());
        // Each CJK glyph should occupy 2 cells in vt100's model.
        assert_eq!(
            src.screen().cell(0, 0).map(|c| c.contents()),
            dst.screen().cell(0, 0).map(|c| c.contents())
        );
    }

    // ─── Zero-width / combining characters ─────────────────────────────────

    #[test]
    fn combining_marks_round_trip() {
        // é as e + combining acute (U+0301).
        let script = "\x1b[1;1Hcaf\u{0065}\u{0301}".as_bytes();
        let (src, dst) = adv_replay(24, 80, script);
        assert_eq!(src.screen().contents(), dst.screen().contents());
    }

    // ─── Cursor past right margin ──────────────────────────────────────────

    #[test]
    fn cursor_at_right_margin_parks_consistently() {
        // Fill exactly to the right margin; cursor should park at col=cols
        // (one-past-last, DECAWM pending-wrap state).
        let script = b"\x1b[1;1Habcde"; // 5 cols grid
        let (src, dst) = adv_replay(4, 5, script);
        assert_eq!(
            src.screen().cursor_position(),
            dst.screen().cursor_position(),
            "pending-wrap cursor position diverges"
        );
    }

    // ─── Tab stops ──────────────────────────────────────────────────────────

    #[test]
    fn custom_tab_stops_surface_in_layout() {
        // Clear all tab stops, set one at col 10, then TAB and write.
        let script = b"\x1b[1;1H\x1b[3g\x1b[1;10H\x1bH\x1b[1;1H\tX";
        let (src, dst) = adv_replay(24, 80, script);
        // The `X` glyph should land at the same column in both parsers.
        assert_eq!(src.screen().contents(), dst.screen().contents());
    }

    // ─── BEL handling ──────────────────────────────────────────────────────

    #[test]
    fn bel_does_not_corrupt_stream() {
        let script = b"before\x07after";
        let (src, dst) = adv_replay(24, 80, script);
        assert_eq!(src.screen().contents(), dst.screen().contents());
        assert!(dst.screen().contents().contains("beforeafter"));
    }

    // ─── Malformed CSI must not corrupt subsequent state ───────────────────

    #[test]
    fn malformed_csi_is_recovered_from() {
        // Garbage in the middle shouldn't prevent the "GOOD" text from
        // rendering at the right position.
        let script = b"\x1b[1;1Hbefore\x1b[~~~invalid\x1b[2;1HGOOD";
        let (src, dst) = adv_replay(24, 80, script);
        assert!(dst.screen().contents().contains("GOOD"));
        assert_eq!(src.screen().contents(), dst.screen().contents());
    }

    // ─── Partial UTF-8 across chunks ────────────────────────────────────────

    #[test]
    fn utf8_split_across_chunks_still_reconstructs() {
        // 4-byte emoji 🚀 split 2/2.
        let rocket = "🚀".as_bytes(); // 4 bytes: 0xF0 0x9F 0x9A 0x80
        assert_eq!(rocket.len(), 4);
        let mut cap = TerminalCapture::new(24, 80);
        cap.feed(b"\x1b[1;1H");
        cap.feed(&rocket[..2]);
        cap.feed(&rocket[2..]);
        cap.feed(b"_tail");
        let snapshot = cap.snapshot_bytes();
        let mut dst = Parser::new(24, 80, SCROLLBACK_LINES);
        dst.process(&snapshot);
        assert!(
            dst.screen().contents().contains("🚀_tail"),
            "replay lost the emoji: {:?}",
            dst.screen().contents()
        );
    }

    // ─── Very long line ────────────────────────────────────────────────────

    #[test]
    fn very_long_line_does_not_overflow_or_crash() {
        let long = vec![b'x'; 10_000];
        let mut cap = TerminalCapture::new(24, 80);
        cap.feed(b"\x1b[1;1H");
        cap.feed(&long);
        // Survives; snapshot is bounded in size regardless of input length.
        let snapshot = cap.snapshot_bytes();
        assert!(
            snapshot.len() < 100_000,
            "snapshot grew unexpectedly: {} bytes",
            snapshot.len()
        );
    }

    // ─── Title (OSC 0/2) survives a reattach ───────────────────────────────

    /// The RED test from #1205. `main` records no title and emits none, so
    /// this fails there; with the sniffer + snapshot re-emission it passes.
    #[test]
    fn window_title_is_re_emitted_in_snapshot() {
        let mut cap = TerminalCapture::new(24, 80);
        cap.feed(b"\x1b]0;myapp-v1\x07\x1b[1;1Hbody");
        assert_eq!(
            cap.sticky.title.as_deref(),
            Some("myapp-v1"),
            "sniffer missed OSC 0 title"
        );
        let snap = cap.snapshot_bytes();
        let needle = b"]0;myapp-v1";
        assert!(
            snap.windows(needle.len()).any(|w| w == needle),
            "snapshot missing the window title. bytes: {:?}",
            String::from_utf8_lossy(&snap)
        );
        // The body text still replays — the title must not disturb cells.
        let (_src, dst) = adv_replay(24, 80, b"\x1b]0;myapp-v1\x07\x1b[1;1Hbody");
        assert!(dst.screen().contents().contains("body"));
    }

    /// OSC 2 (window title only) is normalized to OSC 0 on the way out, and
    /// the ST terminator (`\x1b\\`) is accepted just like BEL.
    #[test]
    fn window_title_osc2_st_terminated_is_recorded() {
        let mut cap = TerminalCapture::new(24, 80);
        cap.feed(b"\x1b]2;pane-title\x1b\\\x1b[1;1Hx");
        assert_eq!(cap.sticky.title.as_deref(), Some("pane-title"));
        let snap = cap.snapshot_bytes();
        let needle = b"]0;pane-title";
        assert!(
            snap.windows(needle.len()).any(|w| w == needle),
            "snapshot missing normalized title. bytes: {:?}",
            String::from_utf8_lossy(&snap)
        );
    }

    /// A second OSC title overwrites the first — the snapshot must reflect
    /// only the latest one.
    #[test]
    fn window_title_last_write_wins() {
        let mut cap = TerminalCapture::new(24, 80);
        cap.feed(b"\x1b]0;first\x07");
        cap.feed(b"\x1b]2;second\x07");
        assert_eq!(cap.sticky.title.as_deref(), Some("second"));
        let snap = cap.snapshot_bytes();
        let stale = b"]0;first";
        assert!(
            !snap.windows(stale.len()).any(|w| w == stale),
            "snapshot still carries the stale title"
        );
    }

    /// The daemon feeds one `feed()` call per PTY read, so an OSC title
    /// split mid-sequence across two calls must still be recorded whole.
    #[test]
    fn window_title_recorded_across_chunk_boundary() {
        let mut cap = TerminalCapture::new(24, 80);
        cap.feed(b"\x1b]0;split");
        cap.feed(b"-title\x07");
        assert_eq!(cap.sticky.title.as_deref(), Some("split-title"));
    }

    /// vte splits the OSC payload on `;`, but a title may legitimately
    /// contain one; `osc_dispatch` rejoins everything after the numeric
    /// kind so the semicolon survives.
    #[test]
    fn window_title_may_contain_semicolons() {
        let mut cap = TerminalCapture::new(24, 80);
        cap.feed(b"\x1b]0;proj: a;b\x07");
        assert_eq!(cap.sticky.title.as_deref(), Some("proj: a;b"));
    }

    /// vte's OSC-string state only ends the string on 0x07/0x18/0x1A/0x1B and
    /// swallows the rest of C0 itself, but DEL and the C1 range are in its
    /// pass-through range and do reach the handler. They must be stripped
    /// rather than replayed verbatim into the client's terminal.
    #[test]
    fn window_title_control_bytes_are_stripped() {
        // Direct check of the sanitizer: C0 (0x00, 0x08, ESC) and DEL are
        // dropped, and the lone 0x9b (a C1 CSI byte, and invalid standalone
        // UTF-8) is lossily decoded to U+FFFD and dropped with it.
        let raw = b"ok\x00\x08\x1b\x7f\x9b[31m-name";
        assert_eq!(sanitize_title(raw), "ok[31m-name");

        // End-to-end through vte: DEL survives vte's OSC state machine, so
        // this really does depend on `sanitize_title` removing it.
        let mut cap = TerminalCapture::new(24, 80);
        cap.feed(b"\x1b]0;tit\x7fle\x1b\\");
        assert_eq!(cap.sticky.title.as_deref(), Some("title"));
        let snap = cap.snapshot_bytes();
        let prefix = snap
            .windows(3)
            .position(|w| w == b"]0;")
            .expect("snapshot missing title prefix");
        let start = prefix + 3;
        let bel = snap[start..]
            .iter()
            .position(|&b| b == 0x07)
            .expect("snapshot missing BEL terminator");
        let emitted = &snap[start..start + bel];
        assert_eq!(emitted, b"title", "title in snapshot was not sanitized");
        assert!(
            emitted.iter().all(|&b| (0x20..0x7f).contains(&b)),
            "title in snapshot still carries a control byte"
        );
    }

    /// An untrusted OSC payload is bounded to `MAX_TITLE_BYTES` so it can't
    /// inflate every snapshot and every reattach payload.
    #[test]
    fn window_title_is_capped() {
        let mut script = b"\x1b]0;".to_vec();
        script.extend_from_slice(&vec![b't'; 4000]);
        script.push(0x07);
        let mut cap = TerminalCapture::new(24, 80);
        cap.feed(&script);
        assert_eq!(
            cap.sticky.title.as_ref().unwrap().len(),
            MAX_TITLE_BYTES,
            "title was not capped"
        );
        let snapshot = cap.snapshot_bytes();
        assert!(
            snapshot.len() < 100_000,
            "snapshot grew unexpectedly: {} bytes",
            snapshot.len()
        );
    }

    /// An explicit empty OSC title (app clearing its title) must be replayed
    /// as an explicit clear, not silently dropped in favor of a stale one.
    #[test]
    fn window_title_cleared_by_empty_osc() {
        let mut cap = TerminalCapture::new(24, 80);
        cap.feed(b"\x1b]0;name\x07\x1b]2;\x07");
        assert_eq!(cap.sticky.title.as_deref(), Some(""));
        let snap = cap.snapshot_bytes();
        let clear = b"\x1b]0;\x07";
        assert!(
            snap.windows(clear.len()).any(|w| w == clear),
            "snapshot missing the explicit title clear. bytes: {:?}",
            String::from_utf8_lossy(&snap)
        );
        let stale = b"]0;name";
        assert!(
            !snap.windows(stale.len()).any(|w| w == stale),
            "snapshot still carries the stale title"
        );
    }

    /// A session that never set a title must not get one synthesized. The
    /// snapshot leaves whatever the attaching client already had alone —
    /// emitting an empty OSC 0 here would blank the user's own title bar.
    #[test]
    fn untitled_session_emits_no_osc_title() {
        let mut cap = TerminalCapture::new(24, 80);
        cap.feed(b"\x1b[1;1Hplain output");
        assert!(cap.sticky.title.is_none(), "title recorded from nothing");
        let snap = cap.snapshot_bytes();
        assert!(
            !snap.windows(3).any(|w| w == b"]0;"),
            "snapshot invented a window title. bytes: {:?}",
            String::from_utf8_lossy(&snap)
        );
    }

    /// Only OSC 0 and OSC 2 carry a window title. OSC 1 (icon name), OSC 8
    /// (hyperlink) and OSC 52 (clipboard) must leave the recorded title
    /// untouched — replaying a clipboard payload into the title bar would be
    /// both wrong and a way to smuggle attacker-chosen bytes into it.
    #[test]
    fn non_title_osc_sequences_are_ignored() {
        let mut cap = TerminalCapture::new(24, 80);
        cap.feed(b"\x1b]0;real\x07");
        cap.feed(b"\x1b]1;icon-only\x07");
        cap.feed(b"\x1b]52;c;c2VjcmV0\x07");
        cap.feed(b"\x1b]8;;https://example.com\x07link\x1b]8;;\x07");
        assert_eq!(cap.sticky.title.as_deref(), Some("real"));
        let snap = cap.snapshot_bytes();
        for stale in [&b"icon-only"[..], &b"c2VjcmV0"[..], &b"example.com"[..]] {
            assert!(
                !snap.windows(stale.len()).any(|w| w == stale),
                "non-title OSC payload leaked into the snapshot: {:?}",
                String::from_utf8_lossy(stale)
            );
        }
    }

    // ─── Resize during alt-screen ──────────────────────────────────────────

    #[test]
    fn resize_while_on_alt_screen_preserves_mode() {
        let mut cap = TerminalCapture::new(24, 80);
        cap.feed(b"\x1b[?1049h\x1b[2J\x1b[1;1HALT");
        cap.resize(40, 120);
        let snapshot = cap.snapshot_bytes();
        let mut dst = Parser::new(40, 120, SCROLLBACK_LINES);
        dst.process(&snapshot);
        assert!(
            dst.screen().alternate_screen(),
            "alt-screen flag lost on resize"
        );
        assert!(dst.screen().contents().contains("ALT"));
    }

    // ─── Mouse protocol mode — input-mode round-trip ───────────────────────

    #[test]
    fn mouse_sgr_mode_replayed() {
        // Enable ButtonMotion + SGR encoding.
        let script = b"\x1b[?1002h\x1b[?1006h\x1b[1;1Hready";
        let (src, dst) = adv_replay(24, 80, script);
        assert_eq!(
            src.screen().mouse_protocol_mode(),
            dst.screen().mouse_protocol_mode(),
            "mouse protocol mode diverges"
        );
        assert_eq!(
            src.screen().mouse_protocol_encoding(),
            dst.screen().mouse_protocol_encoding(),
            "mouse protocol encoding diverges"
        );
    }

    // ─── Bracketed paste mode ──────────────────────────────────────────────

    #[test]
    fn bracketed_paste_mode_replayed() {
        let script = b"\x1b[?2004h\x1b[1;1Hprompt>";
        let (src, dst) = adv_replay(24, 80, script);
        assert_eq!(
            src.screen().bracketed_paste(),
            dst.screen().bracketed_paste()
        );
        assert!(dst.screen().bracketed_paste());
    }

    // ─── Application cursor keys mode ──────────────────────────────────────

    #[test]
    fn application_cursor_mode_replayed() {
        let script = b"\x1b[?1h\x1b[1;1Hreadline";
        let (src, dst) = adv_replay(24, 80, script);
        assert_eq!(
            src.screen().application_cursor(),
            dst.screen().application_cursor()
        );
        assert!(dst.screen().application_cursor());
    }

    // ─── Hidden cursor across alt-screen toggle ────────────────────────────

    #[test]
    fn hidden_cursor_persists_across_alt_toggle() {
        // A TUI typically hides cursor before drawing. That state must
        // survive a detach/reattach cycle regardless of alt-screen flips.
        let script = b"\x1b[?25l\x1b[?1049h\x1b[2J\x1b[1;1HtuiALT";
        let (src, dst) = adv_replay(24, 80, script);
        assert_eq!(src.screen().hide_cursor(), dst.screen().hide_cursor());
        assert!(dst.screen().hide_cursor());
        assert!(dst.screen().alternate_screen());
    }

    // ─── Scroll region (DECSTBM) survives replay, checked by behavior ──────
    //
    // The checks above only compared final frames. This one drives both
    // parsers further AFTER the snapshot round-trip: if the replay restored
    // DECSTBM, a subsequent overflow below the region should not scroll rows
    // outside the region. If the replay lost DECSTBM (RIS resets it, and
    // state_formatted may not re-emit), the source and replay will diverge
    // on the next paint.

    /// Read a row as a trimmed string (for readable assertion diffs).
    fn row_text(parser: &Parser, row: u16) -> String {
        let cols = parser.screen().size().1;
        let s = (0..cols)
            .filter_map(|c| parser.screen().cell(row, c).map(|x| x.contents()))
            .collect::<String>();
        s.trim_end().to_string()
    }

    #[test]
    fn scroll_region_behavior_survives_replay() {
        // Set DECSTBM to rows 3-5 (1-indexed). Paint anchors at row 1 and
        // row 5. After replay, drive a LF at row 5: if DECSTBM is still
        // active, the region scrolls up (row 5 "bottom" moves to row 4,
        // new content lands at row 5). If DECSTBM was lost through RIS,
        // the cursor just advances to row 6 — row 4 stays empty and row 6
        // gets the new content.
        //
        // Assertion: row 4 must contain "bottom" (region scrolled it up) and
        // row 5 must contain "new-last". A divergence here exposes DECSTBM
        // not surviving replay. Tracked in #36.
        let setup = b"\x1b[3;5r\x1b[1;1Hanchor\x1b[5;1Hbottom";
        let (mut src, mut dst) = adv_replay(10, 20, setup);

        let trigger = b"\x1b[5;1H\nnew-last";
        src.process(trigger);
        dst.process(trigger);

        assert_eq!(row_text(&src, 0), "anchor");
        assert_eq!(
            row_text(&src, 3),
            "bottom",
            "source: region must scroll row 5 up"
        );
        assert_eq!(
            row_text(&src, 4),
            "new-last",
            "source: new content on region bottom"
        );

        assert_eq!(row_text(&dst, 0), row_text(&src, 0), "row 1 diverges");
        assert_eq!(
            row_text(&dst, 3),
            row_text(&src, 3),
            "row 4 diverges — DECSTBM not preserved through replay"
        );
        assert_eq!(
            row_text(&dst, 4),
            row_text(&src, 4),
            "row 5 diverges — DECSTBM not preserved through replay"
        );
    }

    // ─── Charset (DEC Special Graphics) survives replay, checked by behavior

    #[test]
    fn dec_graphics_charset_survives_replay() {
        // Switch G0 to DEC Special Graphics. In that mode, ASCII `q` renders
        // as a horizontal line (U+2500). If the replay doesn't restore the
        // charset state, post-replay `q` reverts to the letter q.
        let setup = b"\x1b[1;1H\x1b(0qqq"; // G0 → graphics, write 3 glyphs
        let (mut src, mut dst) = adv_replay(5, 10, setup);

        // Now write 3 more chars in both. If charset is still active, they
        // render as line drawings; if replay dropped charset, they render as
        // plain 'q'.
        src.process(b"qqq");
        dst.process(b"qqq");

        let src_row0 = (0..6u16)
            .filter_map(|c| src.screen().cell(0, c).map(|x| x.contents()))
            .collect::<String>();
        let dst_row0 = (0..6u16)
            .filter_map(|c| dst.screen().cell(0, c).map(|x| x.contents()))
            .collect::<String>();
        assert_eq!(
            src_row0, dst_row0,
            "DEC graphics charset not preserved — post-replay 'q' renders \
             differently in src vs dst (src={:?} dst={:?}). Tracked in #36.",
            src_row0, dst_row0
        );
    }

    // ─── DECSCNM (reverse video screen mode) ───────────────────────────────

    #[test]
    fn decscnm_reverse_video_mode() {
        // Reverse video is a whole-screen flag: it inverts fg/bg for every
        // cell. If the app is in reverse video, the replay must either
        // preserve DECSCNM OR pre-invert every cell's SGR. vt100 doesn't
        // expose DECSCNM as a getter, so we check by rendering behavior:
        // paint with default SGR, enable reverse, then compare cell attrs.
        let script = b"\x1b[1;1Hplain\x1b[?5h";
        let (src, dst) = adv_replay(5, 10, script);
        // We cannot directly observe mode, so we assert cells match.
        assert_eq!(src.screen().contents(), dst.screen().contents());
        // And a post-replay paint should look the same on both sides.
    }

    // ─── Cursor shape (DECSCUSR) — likely a known limitation ───────────────

    #[test]
    fn cursor_shape_limitation_documented() {
        // \x1b[5 q = blinking bar. vt100 0.16 doesn't track cursor shape, so
        // the replay cannot restore it. Documented for future upgrade. If
        // vt100 ever adds cursor_shape(), this test should gain an assertion
        // that it's preserved; for now, just ensure nothing else corrupts.
        let script = b"\x1b[5 q\x1b[1;1Hready";
        let (_src, dst) = adv_replay(5, 10, script);
        assert!(dst.screen().contents().contains("ready"));
    }

    // ─── Detach → write → attach: raw backlog vs grid divergence ───────────

    #[test]
    fn capture_accumulates_while_detached() {
        // Simulate "client detaches, worker keeps running, new output
        // arrives". Multiple feed() calls between snapshots — the grid must
        // reflect the cumulative state, not just the last feed.
        let mut cap = TerminalCapture::new(5, 20);
        cap.feed(b"\x1b[1;1HA");
        cap.feed(b"\x1b[2;1HB");
        cap.feed(b"\x1b[3;1HC");
        let mut dst = Parser::new(5, 20, SCROLLBACK_LINES);
        dst.process(&cap.snapshot_bytes());
        let contents = dst.screen().contents();
        assert!(contents.contains("A"));
        assert!(contents.contains("B"));
        assert!(contents.contains("C"));
    }

    // ─── DECAWM (autowrap) behavior survives replay ────────────────────────

    /// DECAWM is not implemented by vt100 0.16 (only 5 DEC modes are — see
    /// `MODE_*` constants in vt100 screen.rs). That means both source and
    /// replay parsers wrap regardless of `\x1b[?7l`, so we can't verify
    /// end-to-end behavior with vt100 playing both roles. What we CAN verify
    /// is that the sniffer captured DECAWM-off and the snapshot re-emits
    /// `\x1b[?7l` — a real xterm / iTerm / kitty / Windows Terminal on the
    /// client side will then honor it. Locked with a snapshot-bytes check.
    #[test]
    fn decawm_off_is_re_emitted_in_snapshot() {
        let mut cap = TerminalCapture::new(5, 5);
        cap.feed(b"\x1b[?7l\x1b[1;1H");
        assert!(cap.sticky.decawm_off, "sniffer missed DECAWM off");
        let snap = cap.snapshot_bytes();
        assert!(
            snap.windows(5).any(|w| w == b"\x1b[?7l"),
            "snapshot missing \\x1b[?7l. bytes: {:?}",
            String::from_utf8_lossy(&snap)
        );
    }

    /// DECSTBM reset (`\x1b[r` with no params) must clear the sticky region
    /// so the next snapshot doesn't lie about a region the app has cleared.
    #[test]
    fn decstbm_reset_clears_sticky_region() {
        let mut cap = TerminalCapture::new(10, 20);
        cap.feed(b"\x1b[3;7r");
        assert_eq!(cap.sticky.decstbm, Some((3, 7)));
        cap.feed(b"\x1b[r");
        assert_eq!(
            cap.sticky.decstbm, None,
            "DECSTBM reset did not clear sticky region"
        );
    }

    /// Re-enabling autowrap should clear the sticky-off flag so a
    /// subsequent snapshot doesn't lie to the client.
    #[test]
    fn decawm_re_enabled_clears_sticky_flag() {
        let mut cap = TerminalCapture::new(5, 5);
        cap.feed(b"\x1b[?7l\x1b[1;1H");
        assert!(cap.sticky.decawm_off);
        cap.feed(b"\x1b[?7h");
        assert!(!cap.sticky.decawm_off);
        let snap = cap.snapshot_bytes();
        assert!(
            !snap.windows(5).any(|w| w == b"\x1b[?7l"),
            "snapshot wrongly still emits DECAWM-off after app re-enabled wrap"
        );
    }

    // ─── Reverse of the RIS-eats-mode problem: a pre-reset mode must still
    //     be re-asserted after RIS in the snapshot ──────────────────────────

    #[test]
    fn modes_set_before_rendering_are_re_asserted_after_ris() {
        // Bracketed paste mode on, then some rendering. snapshot_bytes starts
        // with \x1bc (RIS) which clears bracketed paste. The replay path must
        // re-emit \x1b[?2004h or the flag gets dropped. This locks in the
        // expectation that our snapshot emits input-mode AFTER the RIS.
        let script = b"\x1b[?2004h\x1b[1;1Hprompt";
        let (src, dst) = adv_replay(5, 20, script);
        assert!(src.screen().bracketed_paste());
        assert!(
            dst.screen().bracketed_paste(),
            "bracketed paste lost — RIS cleared it and snapshot didn't re-emit"
        );
    }
}
