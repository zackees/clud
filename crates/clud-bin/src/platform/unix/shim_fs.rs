//! Unix half of the shim installers' file-mode checks: an alias or real
//! binary counts as runnable only with an execute bit set.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// True when any execute bit (`0o111`) is set.
pub fn has_exec_bit(metadata: &fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o111 != 0
}

/// Set `path`'s permission bits to `mode`.
pub fn set_mode<P: AsRef<Path>>(path: P, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

/// The permission bits of `metadata`.
pub fn mode_bits(metadata: &fs::Metadata) -> Option<u32> {
    Some(metadata.permissions().mode())
}
