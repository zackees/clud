//! Windows console-mode plumbing: for the duration of a PTY session, enable
//! `ENABLE_VIRTUAL_TERMINAL_INPUT` on stdin and
//! `ENABLE_VIRTUAL_TERMINAL_PROCESSING` on stdout, and restore both prior
//! modes on drop. No-op on POSIX.

use std::io;

/// Windows console input mode flag for virtual terminal input.
#[cfg(windows)]
const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

/// Windows console output mode flag for virtual terminal processing.
#[cfg(windows)]
const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

#[cfg(windows)]
extern "system" {
    fn GetConsoleMode(handle: isize, mode: *mut u32) -> i32;
    fn SetConsoleMode(handle: isize, mode: u32) -> i32;
}

/// RAII guard that restores the original console input and output modes on
/// drop.
pub struct ConsoleVtGuard {
    #[cfg(windows)]
    original_mode: Option<u32>,
    #[cfg(windows)]
    original_output_mode: Option<u32>,
}

impl Drop for ConsoleVtGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            if let Some(mode) = self.original_mode {
                restore_console_mode(io::stdin().as_raw_handle() as isize, mode);
            }
            if let Some(mode) = self.original_output_mode {
                restore_console_mode(io::stdout().as_raw_handle() as isize, mode);
            }
        }
    }
}

/// Enable `ENABLE_VIRTUAL_TERMINAL_INPUT` on the Windows console input handle
/// so ANSI sequences (bracketed paste, etc.) pass through to the child
/// process, and `ENABLE_VIRTUAL_TERMINAL_PROCESSING` on the stdout console
/// handle so ANSI sequences written to stdout are interpreted instead of
/// printed literally. Each handle is handled independently: a non-terminal or
/// failing handle does not skip the other.
/// Returns a guard that restores both original modes on drop.
/// On non-Windows platforms this is a no-op.
pub fn enable_console_vt_input() -> ConsoleVtGuard {
    #[cfg(windows)]
    {
        use std::io::IsTerminal;
        use std::os::windows::io::AsRawHandle;
        let original_mode = if io::stdin().is_terminal() {
            or_console_mode(
                io::stdin().as_raw_handle() as isize,
                ENABLE_VIRTUAL_TERMINAL_INPUT,
            )
        } else {
            None
        };
        let original_output_mode = if io::stdout().is_terminal() {
            or_console_mode(
                io::stdout().as_raw_handle() as isize,
                ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            )
        } else {
            None
        };
        ConsoleVtGuard {
            original_mode,
            original_output_mode,
        }
    }
    #[cfg(not(windows))]
    {
        ConsoleVtGuard {}
    }
}

/// OR `bit` into the console mode of `handle`. Returns the original mode, or
/// `None` if the handle is not a console or the mode could not be set.
#[cfg(windows)]
fn or_console_mode(handle: isize, bit: u32) -> Option<u32> {
    unsafe {
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return None;
        }
        let original = mode;
        if SetConsoleMode(handle, mode | bit) == 0 {
            return None;
        }
        Some(original)
    }
}

#[cfg(windows)]
fn restore_console_mode(handle: isize, mode: u32) {
    unsafe {
        SetConsoleMode(handle, mode);
    }
}

/// Check if stdin is a terminal (not piped).
pub fn atty_is_terminal() -> bool {
    use std::io::IsTerminal;
    io::stdin().is_terminal()
}

#[cfg(test)]
mod tests {
    /// Windows: `enable_console_vt_input()` must actually set the
    /// `ENABLE_VIRTUAL_TERMINAL_INPUT` bit (0x0200) on the console input
    /// handle, and restore the original mode on drop. Without this bit,
    /// `ReadConsoleW` delivers Backspace as 0x08 instead of the xterm 0x7f
    /// that Ink-based TUIs (codex) expect, which manifests as "Backspace
    /// doesn't delete anything" inside `clud --codex`.
    ///
    /// Skipped when stdin is not a real console (piped `cargo test`,
    /// CI boxes without an attached TTY).
    #[cfg(windows)]
    #[test]
    fn enable_console_vt_input_sets_and_restores_bit() {
        use super::enable_console_vt_input;
        use std::io::IsTerminal;
        use std::os::windows::io::AsRawHandle;

        const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

        extern "system" {
            fn GetConsoleMode(handle: isize, mode: *mut u32) -> i32;
            fn SetConsoleMode(handle: isize, mode: u32) -> i32;
        }

        if !std::io::stdin().is_terminal() {
            eprintln!(
                "enable_console_vt_input_sets_and_restores_bit: SKIP \
                 (stdin not a real console in this test runner)"
            );
            return;
        }

        let handle = std::io::stdin().as_raw_handle() as isize;
        let saved: u32 = unsafe {
            let mut mode: u32 = 0;
            assert_ne!(GetConsoleMode(handle, &mut mode), 0, "GetConsoleMode");
            mode
        };
        // Clear the VT-input bit so we're starting from a known state.
        unsafe {
            assert_ne!(
                SetConsoleMode(handle, saved & !ENABLE_VIRTUAL_TERMINAL_INPUT),
                0,
                "clear VT input bit"
            );
        }

        let before: u32 = unsafe {
            let mut mode: u32 = 0;
            assert_ne!(GetConsoleMode(handle, &mut mode), 0);
            mode
        };
        assert_eq!(
            before & ENABLE_VIRTUAL_TERMINAL_INPUT,
            0,
            "VT input bit should be cleared at start of test"
        );

        {
            let _guard = enable_console_vt_input();
            let during: u32 = unsafe {
                let mut mode: u32 = 0;
                assert_ne!(GetConsoleMode(handle, &mut mode), 0);
                mode
            };
            assert_ne!(
                during & ENABLE_VIRTUAL_TERMINAL_INPUT,
                0,
                "enable_console_vt_input must set ENABLE_VIRTUAL_TERMINAL_INPUT"
            );
        }

        let after: u32 = unsafe {
            let mut mode: u32 = 0;
            assert_ne!(GetConsoleMode(handle, &mut mode), 0);
            mode
        };
        assert_eq!(
            after & ENABLE_VIRTUAL_TERMINAL_INPUT,
            0,
            "guard must restore the original (cleared) VT input state on drop"
        );

        // Restore the truly-original mode we saved at the top.
        unsafe {
            SetConsoleMode(handle, saved);
        }
    }

    /// Windows (#1345): `enable_console_vt_input()` must also set the
    /// `ENABLE_VIRTUAL_TERMINAL_PROCESSING` bit (0x0004) on the stdout
    /// console handle, and restore the original mode on drop. Without it,
    /// ANSI sequences clud writes to a console with the bit cleared are
    /// printed literally.
    ///
    /// Skipped when stdout is not a real console (captured `cargo test`
    /// output, CI boxes without an attached TTY).
    #[cfg(windows)]
    #[test]
    fn enable_console_vt_input_sets_and_restores_output_vt_processing() {
        use super::enable_console_vt_input;
        use std::io::IsTerminal;
        use std::os::windows::io::AsRawHandle;

        const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

        extern "system" {
            fn GetConsoleMode(handle: isize, mode: *mut u32) -> i32;
            fn SetConsoleMode(handle: isize, mode: u32) -> i32;
        }

        if !std::io::stdout().is_terminal() {
            eprintln!(
                "enable_console_vt_input_sets_and_restores_output_vt_processing: SKIP \
                 (stdout not a real console in this test runner)"
            );
            return;
        }

        let handle = std::io::stdout().as_raw_handle() as isize;
        let saved: u32 = unsafe {
            let mut mode: u32 = 0;
            assert_ne!(GetConsoleMode(handle, &mut mode), 0, "GetConsoleMode");
            mode
        };
        // Clear the VT-processing bit so we're starting from a known state.
        unsafe {
            assert_ne!(
                SetConsoleMode(handle, saved & !ENABLE_VIRTUAL_TERMINAL_PROCESSING),
                0,
                "clear VT processing bit"
            );
        }

        let before: u32 = unsafe {
            let mut mode: u32 = 0;
            assert_ne!(GetConsoleMode(handle, &mut mode), 0);
            mode
        };
        assert_eq!(
            before & ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            0,
            "VT processing bit should be cleared at start of test"
        );

        {
            let _guard = enable_console_vt_input();
            let during: u32 = unsafe {
                let mut mode: u32 = 0;
                assert_ne!(GetConsoleMode(handle, &mut mode), 0);
                mode
            };
            assert_ne!(
                during & ENABLE_VIRTUAL_TERMINAL_PROCESSING,
                0,
                "enable_console_vt_input must set ENABLE_VIRTUAL_TERMINAL_PROCESSING on stdout"
            );
        }

        let after: u32 = unsafe {
            let mut mode: u32 = 0;
            assert_ne!(GetConsoleMode(handle, &mut mode), 0);
            mode
        };
        assert_eq!(
            after & ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            0,
            "guard must restore the original (cleared) VT processing state on drop"
        );

        // Restore the truly-original mode we saved at the top.
        unsafe {
            SetConsoleMode(handle, saved);
        }
    }
}
