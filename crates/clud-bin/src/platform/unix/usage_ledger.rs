//! File creation for [`crate::toast::usage_ledger`].

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// Create `path` for writing, failing if it exists, readable by the
/// owner only (`0o600`).
pub fn create_new_private(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}
