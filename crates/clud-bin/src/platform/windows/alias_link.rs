//! Windows half of `crate::alias_link`: file symlinks, and no std file-index
//! identity.

use std::io;
use std::path::Path;

pub fn symlink(source: &Path, staging: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(source, staging)
}

/// Windows has no stable file-index API in std; a hardlink there is caught by
/// `crate::alias_link::is_fresh`'s size and mtime comparison, and by the byte
/// comparison in `shim_registry`.
pub fn same_file(_a: &std::fs::Metadata, _b: &std::fs::Metadata) -> bool {
    false
}
