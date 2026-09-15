//! Text-cell toast tier and restore-from-shadow (#1189).
//!
//! For terminals without kitty graphics the toast is drawn as styled cells.
//! Unlike an image, cells replace what was there, so removing the toast means
//! repainting the rectangle from clud's `vt100` shadow of the child's intended
//! screen. That is only sound on the **alternate screen**: on the main screen
//! a scroll would push toast cells into the terminal's native scrollback for
//! good. The compositor enforces that restriction.

use vt100::Screen;

use super::Toast;

/// A 0-based cell rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellRect {
    pub row: u16,
    pub col: u16,
    pub width: u16,
    pub height: u16,
}

impl CellRect {
    /// Whether the 1-based terminal coordinate `(x, y)` falls inside.
    pub fn contains_1based(&self, x: u16, y: u16) -> bool {
        let (col, row) = (x.saturating_sub(1), y.saturating_sub(1));
        col >= self.col
            && col < self.col.saturating_add(self.width)
            && row >= self.row
            && row < self.row.saturating_add(self.height)
    }
}

const CLOSE_GLYPH: &str = "✕";
const MIN_TERM_COLS: u16 = 24;

/// Top-right rectangle for a one-row text toast, or `None` if the terminal is
/// too small.
pub fn toast_rect(text: &str, rows: u16, cols: u16) -> Option<CellRect> {
    if cols < MIN_TERM_COLS || rows < 3 {
        return None;
    }
    let text_cols = u16::try_from(text.chars().count()).unwrap_or(u16::MAX);
    // " text  ✕ " : leading space, text, two spaces, glyph, trailing space.
    let width = text_cols.saturating_add(5).min(cols.saturating_sub(2));
    Some(CellRect {
        row: 0,
        col: cols - width - 1,
        width,
        height: 1,
    })
}

/// The close button's cells inside a text toast rectangle.
pub fn close_rect(rect: CellRect) -> CellRect {
    CellRect {
        row: rect.row,
        col: rect.col + rect.width.saturating_sub(3),
        width: 3,
        height: 1,
    }
}

fn severity_sgr(toast: &Toast) -> &'static str {
    match toast.severity {
        super::Severity::Info => "\x1b[0;38;5;255;48;5;24m",
        super::Severity::Warn => "\x1b[0;38;5;232;48;5;214m",
        super::Severity::Alert => "\x1b[0;1;38;5;255;48;5;160m",
    }
}

/// Paint the toast into `rect`. Leaves the cursor and pen wherever the paint
/// ends; the compositor restores both afterwards.
pub fn render_cells(toast: &Toast, rect: CellRect) -> Vec<u8> {
    let inner = usize::from(rect.width).saturating_sub(5);
    let mut text: String = toast.text.chars().take(inner).collect();
    if toast.text.chars().count() > inner && inner > 0 {
        text.pop();
        text.push('…');
    }
    let pad = inner.saturating_sub(text.chars().count());
    let mut out = Vec::new();
    out.extend_from_slice(cup(rect.row, rect.col).as_bytes());
    out.extend_from_slice(severity_sgr(toast).as_bytes());
    out.extend_from_slice(format!(" {text}{}  {CLOSE_GLYPH} ", " ".repeat(pad)).as_bytes());
    out.extend_from_slice(b"\x1b[0m");
    out
}

/// Repaint `rect` from the shadow screen, widened so it never splits a wide
/// character.
pub fn restore_cells(screen: &Screen, rect: CellRect) -> Vec<u8> {
    let (rows, cols) = screen.size();
    let mut out = Vec::new();
    for row in rect.row..rect.row.saturating_add(rect.height).min(rows) {
        let mut start = rect.col.min(cols);
        let mut end = rect.col.saturating_add(rect.width).min(cols);
        if start > 0
            && screen
                .cell(row, start)
                .is_some_and(|c| c.is_wide_continuation())
        {
            start -= 1;
        }
        if end > 0 && end < cols && screen.cell(row, end - 1).is_some_and(|c| c.is_wide()) {
            end += 1;
        }
        if end <= start {
            continue;
        }
        let Some(formatted) = screen
            .rows_formatted(start, end - start)
            .nth(usize::from(row))
        else {
            continue;
        };
        // `rows_formatted` skips blank cells with cursor movement instead of
        // overwriting them, so first erase the segment to default blanks
        // (ECH with a default pen: true blanks, not written spaces), then
        // paint the child's cells over it.
        out.extend_from_slice(cup(row, start).as_bytes());
        out.extend_from_slice(b"\x1b[0m");
        out.extend_from_slice(format!("\x1b[{}X", end - start).as_bytes());
        out.extend_from_slice(&formatted);
    }
    out.extend_from_slice(b"\x1b[0m");
    out
}

/// Repaint every row of the shadow screen.
pub fn restore_screen(screen: &Screen) -> Vec<u8> {
    let (rows, cols) = screen.size();
    restore_cells(
        screen,
        CellRect {
            row: 0,
            col: 0,
            width: cols,
            height: rows,
        },
    )
}

/// 1-based CUP for a 0-based position.
pub fn cup(row: u16, col: u16) -> String {
    format!("\x1b[{};{}H", row + 1, col + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn screen_after(rows: u16, cols: u16, bytes: &[u8]) -> vt100::Parser {
        let mut parser = vt100::Parser::new(rows, cols, 0);
        parser.process(bytes);
        parser
    }

    fn toast(text: &str) -> Toast {
        Toast::new("cpu", text, super::super::Severity::Warn, Instant::now())
    }

    #[test]
    fn the_rect_sits_top_right_inside_the_screen() {
        let rect = toast_rect("cpu 200 %", 24, 80).unwrap();
        assert_eq!(rect.row, 0);
        assert_eq!(rect.col + rect.width, 79);
        assert!(toast_rect("x", 2, 80).is_none());
        assert!(toast_rect("x", 24, 10).is_none());
        let clamped = toast_rect(&"z".repeat(500), 24, 40).unwrap();
        assert_eq!(clamped.width, 38);
    }

    #[test]
    fn rendered_cells_show_the_text_and_close_glyph() {
        let rect = toast_rect("cpu 200 %", 24, 80).unwrap();
        let term = screen_after(24, 80, &render_cells(&toast("cpu 200 %"), rect));
        let row: String = term.screen().rows(rect.col, rect.width).next().unwrap();
        assert!(row.contains("cpu 200 %"), "row: {row:?}");
        assert!(row.contains(CLOSE_GLYPH));
    }

    #[test]
    fn long_text_is_ellipsised_to_the_rect() {
        let rect = toast_rect(&"y".repeat(200), 24, 40).unwrap();
        let term = screen_after(24, 40, &render_cells(&toast(&"y".repeat(200)), rect));
        let row: String = term.screen().rows(rect.col, rect.width).next().unwrap();
        assert!(row.contains('…'));
        assert!(row.contains(CLOSE_GLYPH));
    }

    /// The core guarantee: paint the toast over a styled screen, restore it
    /// from the shadow, and the terminal matches the child's screen again.
    #[test]
    fn restore_brings_back_styled_content_under_the_toast() {
        let child = b"\x1b[1;1H\x1b[31mred text on the first row\x1b[0m and more words here to the right edge....\
\x1b[2;1H\x1b[44m blue background \x1b[0m";
        let shadow = screen_after(6, 60, child);
        let mut terminal = screen_after(6, 60, child);
        let rect = toast_rect("cpu 300 %", 6, 60).unwrap();
        terminal.process(&render_cells(&toast("cpu 300 %"), rect));
        assert_ne!(terminal.screen().contents(), shadow.screen().contents());

        terminal.process(&restore_cells(shadow.screen(), rect));
        assert_eq!(terminal.screen().contents(), shadow.screen().contents());
        for col in 0..60 {
            let (a, b) = (
                terminal.screen().cell(0, col).unwrap(),
                shadow.screen().cell(0, col).unwrap(),
            );
            assert_eq!(a.contents(), b.contents(), "col {col}");
            assert_eq!(a.fgcolor(), b.fgcolor(), "col {col}");
            assert_eq!(a.bgcolor(), b.bgcolor(), "col {col}");
        }
    }

    #[test]
    fn restore_clears_toast_cells_over_blank_space() {
        let shadow = screen_after(6, 40, b"left");
        let mut terminal = screen_after(6, 40, b"left");
        let rect = toast_rect("hello", 6, 40).unwrap();
        terminal.process(&render_cells(&toast("hello"), rect));
        terminal.process(&restore_cells(shadow.screen(), rect));
        assert_eq!(terminal.screen().contents(), shadow.screen().contents());
        assert_eq!(
            terminal.screen().cell(0, 38).unwrap().bgcolor(),
            vt100::Color::Default
        );
    }

    #[test]
    fn restore_never_splits_a_wide_character() {
        // Wide chars straddling the rect's left edge.
        let child = "\x1b[1;1H".to_string() + &"漢".repeat(19) + "x";
        let shadow = screen_after(6, 40, child.as_bytes());
        let mut terminal = screen_after(6, 40, child.as_bytes());
        let rect = CellRect {
            row: 0,
            col: 11,
            width: 10,
            height: 1,
        };
        terminal.process(b"\x1b[1;12H\x1b[7mXXXXXXXXXX\x1b[0m");
        terminal.process(&restore_cells(shadow.screen(), rect));
        assert_eq!(terminal.screen().contents(), shadow.screen().contents());
    }

    #[test]
    fn restore_screen_repaints_everything() {
        let child = b"\x1b[1;1Hone\x1b[3;5Hthree";
        let shadow = screen_after(4, 20, child);
        let mut terminal = screen_after(4, 20, b"\x1b[1;1Hgarbage garbage\x1b[3;1Hmore garbage");
        terminal.process(&restore_screen(shadow.screen()));
        assert_eq!(terminal.screen().contents(), shadow.screen().contents());
    }

    #[test]
    fn close_rect_is_the_trailing_cells_and_hit_testing_is_1_based() {
        let rect = toast_rect("abc", 24, 80).unwrap();
        let close = close_rect(rect);
        assert_eq!(close.col + close.width, rect.col + rect.width);
        assert!(close.contains_1based(close.col + 1, 1));
        assert!(!close.contains_1based(close.col, 1));
        assert!(!close.contains_1based(close.col + 1, 2));
    }
}
