//! Unix half of `crate::block_bad_cmd`'s bounded stdin reader: switching the
//! hook's stdin to `O_NONBLOCK` so the reader can poll it with a deadline.

use std::os::fd::{AsRawFd, RawFd};

/// Stdin switched to non-blocking mode; [`NonblockingStdin::restore`] puts
/// the original file-status flags back.
pub struct NonblockingStdin {
    fd: RawFd,
    old_flags: libc::c_int,
}

impl NonblockingStdin {
    /// Set `O_NONBLOCK` on `stdin`'s descriptor. `None` when the flags
    /// cannot be read or written, so the caller falls back to the threaded
    /// reader.
    pub fn enable(stdin: &std::io::StdinLock<'_>) -> Option<Self> {
        let fd = stdin.as_raw_fd();
        let old_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if old_flags < 0 {
            return None;
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFL, old_flags | libc::O_NONBLOCK) } < 0 {
            return None;
        }
        Some(Self { fd, old_flags })
    }

    /// Restore the file-status flags saved by [`NonblockingStdin::enable`].
    pub fn restore(self) {
        let _ = unsafe { libc::fcntl(self.fd, libc::F_SETFL, self.old_flags) };
    }
}
