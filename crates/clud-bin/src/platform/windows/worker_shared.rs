//! Windows half of `crate::daemon::worker_shared`: the socket liveness probe.
//! The Unix tree documents why POSIX needs a different peek.

use std::io;
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// Probe whether the peer of `stream` has closed the connection, without
/// mutating any flags shared with sibling clones of the same socket.
///
/// On Windows the previous approach is fine — `set_nonblocking` there maps to
/// `ioctlsocket(FIONBIO)`, which is per-handle, so the flag never leaks to
/// sibling clones.
pub fn probe_socket_dead(stream: &TcpStream, attached_at: Instant) -> bool {
    let mut buf = [0u8; 1];
    stream.set_nonblocking(true).ok();
    let dead = match stream.peek(&mut buf) {
        Ok(0) => true,
        Ok(_) => false,
        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => false,
        Err(ref e) if e.kind() == io::ErrorKind::ConnectionReset => true,
        Err(ref e) if e.kind() == io::ErrorKind::ConnectionAborted => true,
        Err(_) => attached_at.elapsed() > Duration::from_secs(10),
    };
    stream.set_nonblocking(false).ok();
    dead
}
