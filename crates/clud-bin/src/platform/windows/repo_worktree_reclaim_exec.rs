//! Windows half of `crate::daemon::gc_service::repo_worktree_reclaim_exec`.

use std::path::Path;

pub fn make_writable(path: &Path, meta: &std::fs::Metadata) {
    let mut perms = meta.permissions();
    if perms.readonly() {
        // Windows has no mode bits: this only clears FILE_ATTRIBUTE_READONLY.
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        let _ = std::fs::set_permissions(path, perms);
    }
}
