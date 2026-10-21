//! Filesystem link for [`crate::video`].

use std::path::Path;

/// Link the directory `link` to `target` with a symlink.
pub fn link_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}
