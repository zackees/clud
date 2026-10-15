//! #1942: during a PTY session, a full-width row must defer its wrap on the
//! real Windows console, as it does on a VT terminal.
//!
//! clud relays a fullscreen TUI's output to its own stdout console. Without
//! `DISABLE_NEWLINE_AUTO_RETURN`, conhost wraps eagerly. Writing the last
//! column moves the cursor to the next line, and on the bottom row scrolls
//! the whole screen. The child assumed a deferred wrap, so every full-width
//! row it drew (separators, a panel reaching the right edge) shifted the
//! terminal by a line it never repaired. The user saw fragments of
//! neighbouring lines in columns 0-1, and Windows Terminal's buffer
//! disagreed with what Claude Code believed it had drawn.

use std::io::Write;

use winapi::um::processenv::GetStdHandle;
use winapi::um::winbase::STD_OUTPUT_HANDLE;
use winapi::um::wincon::{GetConsoleScreenBufferInfo, CONSOLE_SCREEN_BUFFER_INFO};

const DISABLE_NEWLINE_AUTO_RETURN: u32 = 0x0008;

fn buffer_info() -> CONSOLE_SCREEN_BUFFER_INFO {
    // SAFETY: the std output handle is the process's own console handle and
    // `info` is a valid out-pointer.
    unsafe {
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        assert_ne!(
            GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info),
            0,
            "GetConsoleScreenBufferInfo"
        );
        info
    }
}

fn output_mode() -> u32 {
    let mut mode = 0u32;
    // SAFETY: as above.
    unsafe {
        assert_ne!(
            winapi::um::consoleapi::GetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), &mut mode),
            0,
            "GetConsoleMode"
        );
    }
    mode
}

#[test]
fn a_full_width_bottom_row_defers_its_wrap_during_a_pty_session() {
    require_pty_or_skip!("a_full_width_bottom_row_defers_its_wrap_during_a_pty_session");

    let original = output_mode();
    {
        let _guard = clud::console_setup::enable_console_vt_input();
        assert_ne!(
            output_mode() & DISABLE_NEWLINE_AUTO_RETURN,
            0,
            "a PTY session must defer end-of-line wrap on the stdout console"
        );

        let window = buffer_info().srWindow;
        let width = window.Right - window.Left + 1;
        let bottom = window.Bottom - window.Top + 1;
        let mut out = std::io::stdout().lock();
        write!(out, "\x1b[{bottom};1H{}", "x".repeat(width as usize)).unwrap();
        out.flush().unwrap();
        drop(out);

        let after = buffer_info();
        assert_eq!(
            (after.dwCursorPosition.X, after.dwCursorPosition.Y),
            (width - 1, window.Bottom),
            "a full-width row must leave the cursor pending in the last column, \
             not wrap it to the next line"
        );
        assert_eq!(
            after.srWindow.Top, window.Top,
            "a full-width bottom row must not scroll the screen"
        );
        // Leave a clean line for whatever the harness prints next.
        print!("\r\n");
    }
    assert_eq!(
        output_mode() & DISABLE_NEWLINE_AUTO_RETURN,
        original & DISABLE_NEWLINE_AUTO_RETURN,
        "the guard must restore the original wrap mode on drop"
    );
}
