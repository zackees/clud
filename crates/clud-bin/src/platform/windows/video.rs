//! Filesystem link for [`crate::video`].

use std::path::Path;

/// Link the directory `link` to `target` with a directory symlink.
pub fn link_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}
