//! Unix half of `crate::daemon::worker_shared`: the socket liveness probe.

use std::io;
use std::net::TcpStream;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

/// Probe whether the peer of `stream` has closed the connection, without
/// mutating any flags shared with sibling clones of the same socket.
///
/// On Unix, `TcpStream::set_nonblocking` calls `fcntl(F_SETFL, O_NONBLOCK)`,
/// which sets the flag on the file *description* — shared with every fd
/// produced by `dup()`/`try_clone()`. The previous probe implementation flipped
/// nonblocking-on, peeked, then flipped it back, but during that brief window
/// the writer thread's clone of the same socket would also be nonblocking. A
/// concurrent `write_all` could then fail with `WouldBlock`, the writer thread
/// would break, `detach_client` would clear the slot, and an in-flight second
/// attach would race in and either succeed or see EOF before the rejection
/// reached the wire. That's the flake the macOS ARM integration test
/// (`test_concurrent_attach_attempt_is_rejected`) was hitting.
///
/// Using `recv(MSG_DONTWAIT | MSG_PEEK)` directly performs a one-shot
/// nonblocking peek without modifying any persistent socket state, so sibling
/// fds keep their original (blocking) mode throughout.
///
/// On Windows the previous approach is fine — `set_nonblocking` there maps to
/// `ioctlsocket(FIONBIO)`, which is per-handle, so the flag never leaks to
/// sibling clones.
pub fn probe_socket_dead(stream: &TcpStream, attached_at: Instant) -> bool {
    let fd = stream.as_raw_fd();
    let mut buf = [0u8; 1];
    // SAFETY: `fd` is a valid socket descriptor for the lifetime of the
    // borrowed `&TcpStream`. `recv` writes at most `buf.len()` bytes into
    // `buf` and never reads from the rest of the address space.
    let ret = unsafe {
        libc::recv(
            fd,
            buf.as_mut_ptr() as *mut _,
            buf.len(),
            libc::MSG_DONTWAIT | libc::MSG_PEEK,
        )
    };
    if ret == 0 {
        return true; // peer closed (clean EOF)
    }
    if ret > 0 {
        return false; // data available, definitely alive
    }
    let err = io::Error::last_os_error();
    match err.kind() {
        io::ErrorKind::WouldBlock => false,
        io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted => true,
        // Unknown error: treat as stale after 10s, matches the prior policy.
        _ => attached_at.elapsed() > Duration::from_secs(10),
    }
}
