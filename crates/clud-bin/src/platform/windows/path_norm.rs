//! Windows half of `crate::path_norm`'s host-specific test support.

/// A second spelling of `real` for `same_path` tests. Windows runners may
/// lack the symlink privilege, so this is the real spelling itself.
#[cfg(test)]
pub(crate) fn test_alias(real: &std::path::Path, _alias: &std::path::Path) -> std::path::PathBuf {
    real.to_path_buf()
}
