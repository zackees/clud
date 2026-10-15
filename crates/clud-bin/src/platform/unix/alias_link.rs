//! Unix half of `crate::alias_link`: POSIX symlinks and dev/inode identity.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

pub fn symlink(source: &Path, staging: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(source, staging)
}

/// The same file when the device and inode match (a hardlink).
pub fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}
