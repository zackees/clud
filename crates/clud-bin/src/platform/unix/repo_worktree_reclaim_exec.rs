//! Unix half of `crate::daemon::gc_service::repo_worktree_reclaim_exec`.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub fn make_writable(path: &Path, meta: &std::fs::Metadata) {
    // Unlinking needs write on the parent directory only; files are fine.
    if !meta.is_dir() {
        return;
    }
    let mode = meta.permissions().mode();
    if mode & 0o700 != 0o700 {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o700));
    }
}
