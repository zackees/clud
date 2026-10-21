//! Unix half of `crate::daemon::attach_input`'s byte source: stdin itself,
//! polled so the source can stop cleanly before the continue-in-background
//! prompt reads the terminal.

use std::io;
use std::time::Duration;

/// The raw terminal input source for one interactive attach.
pub struct RawInputSource {
    fd: std::os::fd::RawFd,
}

impl RawInputSource {
    /// Start reading the real terminal.
    pub fn start() -> Self {
        use std::os::fd::AsRawFd;
        Self::from_fd(io::stdin().as_raw_fd())
    }

    fn from_fd(fd: std::os::fd::RawFd) -> Self {
        Self { fd }
    }

    /// POSIX stdin is a byte stream: the attach expands Ctrl+V itself.
    pub fn expands_ctrl_v(&self) -> bool {
        true
    }

    /// Wait up to `timeout` for the next chunk: `Some(Some(chunk))` for
    /// bytes, `Some(None)` when idle, `None` once the source closed.
    pub fn poll(&mut self, timeout: Duration) -> Option<Option<Vec<u8>>> {
        poll_fd(self.fd, timeout)
    }
}

/// Wait for `fd` to become readable, then read what is there. Polling
/// instead of a blocked reader thread means nothing is left reading the
/// terminal once the attach loop returns.
fn poll_fd(fd: std::os::fd::RawFd, timeout: Duration) -> Option<Option<Vec<u8>>> {
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let timeout_ms = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: `pollfd` is a valid, exclusively borrowed array of length 1.
    let ready = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
    if ready < 0 {
        return if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            Some(None)
        } else {
            None
        };
    }
    if ready == 0 {
        return Some(None);
    }
    let mut buf = [0u8; 4096];
    // SAFETY: `buf` is valid for writes of `buf.len()` bytes.
    let read = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    match read {
        0 => None,
        n if n < 0 => match io::Error::last_os_error().kind() {
            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => Some(None),
            _ => None,
        },
        n => Some(Some(buf[..n as usize].to_vec())),
    }
}

/// Test seam: a source reading stdin's descriptor, as `start` builds it.
#[cfg(test)]
pub fn stdin_fd_source_for_test() -> Option<RawInputSource> {
    Some(RawInputSource::from_fd(0))
}

/// Test seam: a source on the read end of a fresh pipe, plus the write end
/// and the read end's owner (the source holds only the raw descriptor).
#[cfg(test)]
pub fn pipe_source_for_test() -> Option<(RawInputSource, std::fs::File, std::fs::File)> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` is a valid array of two descriptors for pipe(2).
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    // SAFETY: pipe(2) returned two fresh descriptors this test owns.
    let (read_end, write_end) =
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    let source = RawInputSource::from_fd(read_end.as_raw_fd());
    Some((
        source,
        std::fs::File::from(write_end),
        std::fs::File::from(read_end),
    ))
}

/// Test seam: the Windows console reader does not exist here.
#[cfg(test)]
pub fn injected_ctrl_v_console_source_for_test() -> Option<RawInputSource> {
    None
}
