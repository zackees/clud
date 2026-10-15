//! Windows half of `crate::optimize`.

use std::path::Path;

/// Windows has no execute bit to set.
pub fn make_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}
