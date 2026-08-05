//! Compatibility checks for clients talking to older daemon instances.

use std::io;

pub(super) fn is_old_daemon_signature(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    )
}
