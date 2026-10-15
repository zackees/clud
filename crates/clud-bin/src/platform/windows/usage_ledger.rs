//! File creation for [`crate::toast::usage_ledger`].

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Create `path` for writing, failing if it exists. Windows has no POSIX
/// mode bits; the file inherits the directory's ACL.
pub fn create_new_private(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}
