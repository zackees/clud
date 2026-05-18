//! Windows console-mode plumbing: enable `ENABLE_VIRTUAL_TERMINAL_INPUT` for
//! the duration of a PTY session, and restore the prior mode on drop. No-op
//! on POSIX.

use std::io;

/// RAII guard that restores the original console input mode on drop.
pub struct ConsoleVtGuard {
    #[cfg(windows)]
    original_mode: Option<u32>,
}

impl Drop for ConsoleVtGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(mode) = self.original_mode {
            restore_console_mode(mode);
        }
    }
}

/// Enable `ENABLE_VIRTUAL_TERMINAL_INPUT` on the Windows console so ANSI
/// sequences (bracketed paste, etc.) pass through to the child process.
/// Returns a guard that restores the original mode on drop.
/// On non-Windows platforms this is a no-op.
pub fn enable_console_vt_input() -> ConsoleVtGuard {
    #[cfg(windows)]
    {
        use std::io::IsTerminal;
        if !io::stdin().is_terminal() {
            return ConsoleVtGuard {
                original_mode: None,
            };
        }
        match set_console_vt_input(true) {
            Some(original) => ConsoleVtGuard {
                original_mode: Some(original),
            },
            None => ConsoleVtGuard {
                original_mode: None,
            },
        }
    }
    #[cfg(not(windows))]
    {
        ConsoleVtGuard {}
    }
}

#[cfg(windows)]
fn set_console_vt_input(enable: bool) -> Option<u32> {
    use std::os::windows::io::AsRawHandle;

    // Windows console mode flag for virtual terminal input processing.
    const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

    extern "system" {
        fn GetConsoleMode(handle: isize, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: isize, mode: u32) -> i32;
    }

    let handle = io::stdin().as_raw_handle() as isize;
    unsafe {
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return None;
        }
        let original = mode;
        if enable {
            mode |= ENABLE_VIRTUAL_TERMINAL_INPUT;
        } else {
            mode &= !ENABLE_VIRTUAL_TERMINAL_INPUT;
        }
        if SetConsoleMode(handle, mode) == 0 {
            return None;
        }
        Some(original)
    }
}

#[cfg(windows)]
fn restore_console_mode(mode: u32) {
    use std::os::windows::io::AsRawHandle;

    extern "system" {
        fn SetConsoleMode(handle: isize, mode: u32) -> i32;
    }

    let handle = io::stdin().as_raw_handle() as isize;
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
}
