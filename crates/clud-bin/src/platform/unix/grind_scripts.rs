//! Unix half of `crate::command::grind_scripts`.

/// Whether any execute bit is set.
pub fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}
