//! Raw-mode stdin for the mock agent on Unix (`termios`).

use std::io;

/// Put stdin into raw mode (no canonical line buffering, no echo) when it is
/// a terminal. No-op on non-Unix or when stdin is a pipe.
pub fn set_stdin_raw_if_tty() {
    use std::os::unix::io::AsRawFd;
    let fd = io::stdin().as_raw_fd();
    if unsafe { libc::isatty(fd) } == 0 {
        return;
    }
    let mut termios: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut termios) } != 0 {
        return;
    }
    unsafe { libc::cfmakeraw(&mut termios) };
    let _ = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios) };
}
