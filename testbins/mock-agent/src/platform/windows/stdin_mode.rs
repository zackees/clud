//! Raw (VT) console input for the mock agent on Windows.

/// Windows twin of the POSIX `cfmakeraw` in `platform/unix/stdin_mode.rs` (#1310). A console TUI such as
/// Claude Code or Codex switches its input to VT mode: `ENABLE_VIRTUAL_
/// TERMINAL_INPUT` on, line/echo/processed input off. Left in the default
/// cooked mode, conhost turns the escape sequences the pump forwards into
/// key events that `ReadFile` drops, and line-buffers input with `\r\n`, so
/// the mock never sees the bytes a real child would.
pub fn set_stdin_raw_if_tty() {
    use std::ffi::c_void;
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
    const ENABLE_LINE_INPUT: u32 = 0x0002;
    const ENABLE_ECHO_INPUT: u32 = 0x0004;
    const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(std_handle: u32) -> *mut c_void;
        fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: *mut c_void, mode: u32) -> i32;
        fn GetLastError() -> u32;
    }
    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let mut mode = 0u32;
    if handle.is_null() || unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
        crate::trace("stdin console mode: not a console");
        return; // not a console: a pipe needs no mode change
    }
    let raw = (mode & !(ENABLE_PROCESSED_INPUT | ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT))
        | ENABLE_VIRTUAL_TERMINAL_INPUT;
    let ok = unsafe { SetConsoleMode(handle, raw) };
    let error = if ok == 0 {
        unsafe { GetLastError() }
    } else {
        0
    };
    let mut after = 0u32;
    unsafe { GetConsoleMode(handle, &mut after) };
    crate::trace(&format!(
        "stdin console mode {mode:#06x} -> requested {raw:#06x}, set ok={ok} err={error}, now {after:#06x}"
    ));
}
