//! Unix half of `crate::path_norm`'s host-specific test support.

/// A second spelling of `real` for `same_path` tests: a symlink at `alias`.
#[cfg(test)]
pub(crate) fn test_alias(real: &std::path::Path, alias: &std::path::Path) -> std::path::PathBuf {
    std::os::unix::fs::symlink(real, alias).unwrap();
    alias.to_path_buf()
}
