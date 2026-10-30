//! Windows half of `crate::platform::file_identity`: no std file-index API,
//! so callers fall back to their non-inode checks.

use std::path::Path;

/// Always `None`: std exposes no stable file index on Windows.
pub fn file_identity(_path: &Path) -> Option<(u64, u64)> {
    None
}
