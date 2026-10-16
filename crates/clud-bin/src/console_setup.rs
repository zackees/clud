//! Windows console-mode plumbing. At startup, enable
//! `ENABLE_VIRTUAL_TERMINAL_PROCESSING` on the stdout and stderr consoles for
//! the life of the process (#1374). For the duration of a PTY session, also
//! enable `ENABLE_VIRTUAL_TERMINAL_INPUT` on stdin and re-assert VT
//! processing on stdout, restoring both prior modes on drop. No-op on POSIX.

use std::io;

/// Windows console input mode flag for virtual terminal input.
pub const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

/// Windows console output mode flag for virtual terminal processing.
pub const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

/// Windows console output mode flag that defers the end-of-line wrap (#1942).
///
/// Without it, conhost moves the cursor to the next line, and on the bottom
/// row scrolls the screen, as soon as a character lands in the last column.
/// A VT terminal defers that wrap until the next printable character. A
/// fullscreen TUI relayed through the PTY draws full-width rows and then
/// positions the cursor, assuming the deferred wrap. Each eager wrap shifts
/// the real screen by a line the child does not know about, and its
/// cell-diff renderer never repairs the shift.
pub const DISABLE_NEWLINE_AUTO_RETURN: u32 = 0x0008;

/// The stdout bits a PTY session needs: VT processing plus the VT wrap
/// semantics the relayed child assumes.
pub const PTY_SESSION_OUTPUT_MODE_BITS: u32 =
    ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN;

/// A standard stream clud writes escape sequences to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputStream {
    Stdout,
    Stderr,
}

/// Console-mode access for the output streams. Windows implements it with
/// `Get/SetConsoleMode`; tests substitute a fake so the enable logic runs on
/// every platform.
pub(crate) trait OutputConsoleModes {
    /// The stream's console mode, or `None` when it is not a console (a pipe,
    /// a file, or any stream off Windows).
    fn get(&self, stream: OutputStream) -> Option<u32>;
    fn set(&mut self, stream: OutputStream, mode: u32);
}

/// Enable `ENABLE_VIRTUAL_TERMINAL_PROCESSING` on every standard output
/// stream that is a console, so the escape sequences clud writes (colored
/// notices, selector frames, graphics headers, relayed child output) are
/// interpreted instead of printed literally.
///
/// `main` calls this before dispatching anything, so no launch path depends
/// on some earlier code having enabled it (#1374). It is deliberately not
/// restored at exit: clud writes escape sequences for its whole life,
/// including after a PTY session ends and on `process::exit` paths where no
/// destructor runs. Cheap and idempotent, so a selector calls it again in
/// case a child sharing the console cleared the bit. No-op on POSIX.
pub fn enable_console_vt_output() {
    enable_vt_processing(&mut crate::platform::console_setup::StdConsole);
}

/// OR VT processing into each console stream that lacks it. Stdout and stderr
/// usually share one screen buffer, so the second stream then already has it
/// and is left alone.
pub(crate) fn enable_vt_processing(console: &mut impl OutputConsoleModes) {
    for stream in [OutputStream::Stdout, OutputStream::Stderr] {
        if let Some(mode) = console.get(stream) {
            if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING == 0 {
                console.set(stream, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
            }
        }
    }
}

/// Enable `ENABLE_VIRTUAL_TERMINAL_INPUT` on the Windows console input handle
/// so ANSI sequences (bracketed paste, etc.) pass through to the child
/// process, and [`PTY_SESSION_OUTPUT_MODE_BITS`] on the stdout console handle
/// so ANSI sequences written to stdout are interpreted instead of printed
/// literally, and a full-width row defers its wrap as on a VT terminal
/// (#1942). Each handle is handled independently: a non-terminal or
/// failing handle does not skip the other.
/// Returns a guard that restores both original modes on drop.
/// On non-Windows platforms this is a no-op.
pub use crate::platform::console_setup::{enable_console_vt_input, ConsoleVtGuard};

/// Check if stdin is a terminal (not piped).
pub fn atty_is_terminal() -> bool {
    use std::io::IsTerminal;
    io::stdin().is_terminal()
}

#[cfg(test)]
mod tests {
    /// #1374: VT output processing is enabled explicitly, first thing in
    /// `main`, so every launch path gets it. It used to come only from
    /// crossterm's `supports_ansi`, which the selector called: a `Once`-latched
    /// side effect that never undoes itself. Escape sequences clud writes
    /// outside the PTY session's guard (colored notices, the graphics header,
    /// early relayed attach output) therefore rendered on a launch that showed
    /// a picker and printed as literal text on an ordinary repeat launch. That
    /// side effect hid the missing enable whenever a selector ran first.
    #[test]
    fn vt_output_is_enabled_at_startup_not_by_a_selector_side_effect() {
        let main_rs = include_str!("main.rs");
        let main_body = main_rs
            .split("\nfn main() {\n")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .expect("main.rs defines `fn main`");
        assert!(
            main_body.contains("console_setup::enable_console_vt_output();"),
            "`fn main` must enable VT output processing before dispatching any launch path"
        );

        for (name, source) in [
            ("main.rs", main_rs),
            ("selector.rs", include_str!("selector.rs")),
            ("harness_picker.rs", include_str!("harness_picker.rs")),
            ("launch_setup.rs", include_str!("launch_setup.rs")),
            ("settings_tui.rs", include_str!("settings_tui.rs")),
            (
                "foreground_runtime.rs",
                include_str!("foreground_runtime.rs"),
            ),
            (
                "session_history/picker.rs",
                include_str!("session_history/picker.rs"),
            ),
        ] {
            let production = source.split("#[cfg(test)]").next().unwrap_or(source);
            assert!(
                !production.contains("ansi_support::"),
                "{name} relies on crossterm's Once-latched VT enable; \
                 call console_setup::enable_console_vt_output instead"
            );
        }
    }

    use super::{
        enable_vt_processing, OutputConsoleModes, OutputStream, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    };

    /// A console where each stream is a screen buffer (`Some(index)`) or not
    /// a console at all (`None`). Streams that share an index share a mode,
    /// like stdout and stderr in one console window.
    struct FakeConsole {
        buffers: Vec<u32>,
        stdout: Option<usize>,
        stderr: Option<usize>,
        sets: Vec<OutputStream>,
    }

    impl FakeConsole {
        fn buffer(&self, stream: OutputStream) -> Option<usize> {
            match stream {
                OutputStream::Stdout => self.stdout,
                OutputStream::Stderr => self.stderr,
            }
        }
    }

    impl OutputConsoleModes for FakeConsole {
        fn get(&self, stream: OutputStream) -> Option<u32> {
            self.buffer(stream).map(|index| self.buffers[index])
        }

        fn set(&mut self, stream: OutputStream, mode: u32) {
            let index = self.buffer(stream).expect("set on a non-console stream");
            self.buffers[index] = mode;
            self.sets.push(stream);
        }
    }

    const LEGACY_OUTPUT_MODE: u32 = 0x0003; // processed output + wrap at EOL

    #[test]
    fn enables_vt_processing_on_each_console_stream_and_keeps_other_bits() {
        let mut console = FakeConsole {
            buffers: vec![LEGACY_OUTPUT_MODE, LEGACY_OUTPUT_MODE],
            stdout: Some(0),
            stderr: Some(1),
            sets: Vec::new(),
        };
        enable_vt_processing(&mut console);
        let enabled = LEGACY_OUTPUT_MODE | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        assert_eq!(console.buffers, vec![enabled, enabled]);
    }

    #[test]
    fn a_shared_screen_buffer_is_set_once() {
        let mut console = FakeConsole {
            buffers: vec![LEGACY_OUTPUT_MODE],
            stdout: Some(0),
            stderr: Some(0),
            sets: Vec::new(),
        };
        enable_vt_processing(&mut console);
        assert_eq!(
            console.buffers[0],
            LEGACY_OUTPUT_MODE | ENABLE_VIRTUAL_TERMINAL_PROCESSING
        );
        assert_eq!(console.sets, vec![OutputStream::Stdout]);
    }

    /// Redirected stdout (`clud ... > log`) still gets VT processing on the
    /// console stderr, where clud's colored notices and one selector go.
    #[test]
    fn a_redirected_stream_is_skipped_without_skipping_the_other() {
        let mut console = FakeConsole {
            buffers: vec![LEGACY_OUTPUT_MODE],
            stdout: None,
            stderr: Some(0),
            sets: Vec::new(),
        };
        enable_vt_processing(&mut console);
        assert_eq!(console.sets, vec![OutputStream::Stderr]);
    }

    #[test]
    fn a_mode_that_already_has_vt_processing_is_not_rewritten() {
        let enabled = LEGACY_OUTPUT_MODE | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        let mut console = FakeConsole {
            buffers: vec![enabled, enabled],
            stdout: Some(0),
            stderr: Some(1),
            sets: Vec::new(),
        };
        enable_vt_processing(&mut console);
        assert!(console.sets.is_empty());
    }
}
