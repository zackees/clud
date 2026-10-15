//! Unix half of `crate::optimize`.

use std::fs;
use std::path::Path;

/// `chmod 0o755` the installed binary.
pub fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)
        .map_err(|error| format!("metadata {}: {error}", path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .map_err(|error| format!("chmod {}: {error}", path.display()))
}
