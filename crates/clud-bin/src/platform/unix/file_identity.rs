//! Unix half of `crate::platform::file_identity`: `(dev, ino)` of a path.

use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// `(dev, ino)` of the file at `path` (not following a final symlink), or
/// `None` when it cannot be inspected.
pub fn file_identity(path: &Path) -> Option<(u64, u64)> {
    std::fs::symlink_metadata(path)
        .ok()
        .map(|m| (m.dev(), m.ino()))
}
