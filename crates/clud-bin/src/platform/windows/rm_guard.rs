//! Windows half of `crate::rm_guard`: a junction, mount point or other
//! reparse point stands in for a Unix mount boundary.

use std::path::Path;

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

pub fn is_mount_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    std::fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

pub fn contains_nested_mount(path: &Path) -> Result<bool, String> {
    windows_reparse_tree(path)
}

/// Whether `path` or anything under it is a reparse point.
pub(crate) fn windows_reparse_tree(path: &Path) -> Result<bool, String> {
    use std::os::windows::fs::MetadataExt;
    let mut pending = vec![path.to_path_buf()];
    while let Some(item) = pending.pop() {
        let meta = std::fs::symlink_metadata(&item)
            .map_err(|error| format!("cannot inspect deletion tree {item:?}: {error}"))?;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Ok(true);
        }
        if !meta.is_dir() {
            continue;
        }
        let entries = std::fs::read_dir(&item)
            .map_err(|error| format!("cannot inspect deletion tree {item:?}: {error}"))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("cannot inspect deletion tree: {error}"))?;
            pending.push(entry.path());
        }
    }
    Ok(false)
}
