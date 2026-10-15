//! Unix helpers for `crate::runner`'s tests.

/// Mark a test fixture file executable (`0o755`).
#[cfg(test)]
pub(crate) fn mark_executable_for_test(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
