//! Unix half of `crate::codex_auth`: credential files are created mode 0600.

use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// Create the credential file owner read/write only.
pub fn restrict_credential_open_options(options: &mut OpenOptions) {
    options.mode(0o600);
}

/// No-op off Windows: the 0600 mode set at open time already restricts the
/// file to its owner.
pub fn protect_windows_credential_file(_path: &Path) -> Result<(), String> {
    Ok(())
}
