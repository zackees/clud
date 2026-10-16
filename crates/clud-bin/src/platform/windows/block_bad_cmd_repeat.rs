//! Windows half of `crate::block_bad_cmd`'s repeat-streak state file.

/// No POSIX mode bits on Windows; the file keeps its default ACL.
pub fn set_owner_only_mode(_options: &mut std::fs::OpenOptions) {}
