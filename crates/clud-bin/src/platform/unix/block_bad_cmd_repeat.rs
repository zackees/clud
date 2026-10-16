//! Unix half of `crate::block_bad_cmd`'s repeat-streak state file.

use std::os::unix::fs::OpenOptionsExt;

/// Create the streak file owner-only (`0o600`).
pub fn set_owner_only_mode(options: &mut std::fs::OpenOptions) {
    options.mode(0o600);
}
