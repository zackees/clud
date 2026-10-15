//! Windows stubs for the test-only POSIX filesystem fixtures (see the Unix
//! tree). Every test that would call them returns early off Unix.

use std::ffi::OsString;
use std::io;
use std::path::Path;

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "POSIX test fixture is Unix-only",
    )
}

pub fn set_mode(_path: impl AsRef<Path>, _mode: u32) -> io::Result<()> {
    Err(unsupported())
}

pub fn symlink(_target: impl AsRef<Path>, _link: impl AsRef<Path>) -> io::Result<()> {
    Err(unsupported())
}

/// `None`: Windows names are UTF-16, not arbitrary bytes.
pub fn os_string_from_bytes(_bytes: Vec<u8>) -> Option<OsString> {
    None
}
