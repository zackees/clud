//! Unix half of `crate::gc::session_tmp_continuation`: mode bits and the
//! raw OS bytes of a path for the resumable cursor.

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub fn make_owner_writable(dir: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(dir) else {
        return false;
    };
    if !meta.is_dir() {
        return false;
    }
    let mode = meta.permissions().mode();
    if mode & 0o300 == 0o300 {
        return false;
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(mode | 0o300)).is_ok()
}

pub fn path_bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}

pub fn bytes_path(bytes: &[u8]) -> Result<PathBuf, &'static str> {
    Ok(OsString::from_vec(bytes.to_vec()).into())
}
