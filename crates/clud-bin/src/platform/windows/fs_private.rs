//! Windows half of `crate::fs_private`: owner-only files get the protected
//! DACL that `codex_auth` uses for credentials.

use std::fs::OpenOptions;
use std::io;
use std::path::Path;

/// Windows has no creation mode bits; the DACL is applied after opening.
pub fn owner_only_mode(_options: &mut OpenOptions) {}

/// Give `path` the owner-only protected DACL.
pub fn protect_owner_only(path: &Path) -> io::Result<()> {
    crate::codex_auth::protect_windows_credential_file(path).map_err(io::Error::other)
}
