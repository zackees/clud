//! Platform-aware home-directory lookup for skill installation.

use std::path::PathBuf;

pub(super) fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    if let Some(path) = std::env::var_os("USERPROFILE").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }

    std::env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}
