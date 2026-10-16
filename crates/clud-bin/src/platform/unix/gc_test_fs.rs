//! Test-only POSIX filesystem fixtures for the gc / daemon sweep tests:
//! mode bits, symlinks and non-UTF-8 names. Their tests return early off
//! Unix, so the Windows tree only stubs these.

use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// `chmod path mode`.
pub fn set_mode(path: impl AsRef<Path>, mode: u32) -> io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// Create a symlink at `link` pointing to `target`.
pub fn symlink(target: impl AsRef<Path>, link: impl AsRef<Path>) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// An `OsString` holding exactly these bytes (which may be invalid UTF-8).
pub fn os_string_from_bytes(bytes: Vec<u8>) -> Option<OsString> {
    Some(OsString::from_vec(bytes))
}
