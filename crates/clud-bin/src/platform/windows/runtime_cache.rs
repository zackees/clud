//! Windows half of `crate::runtime_cache`: Windows has no `exec`, so the
//! cache hop relays to the cached binary and exits with its code.

use std::ffi::OsString;
use std::io;
use std::path::Path;

/// Run the cached binary through [`crate::trampoline::relay_child_and_wait`]
/// and exit with its code. Returns only on spawn failure.
pub fn reexec_from_cached_binary(cached: &Path, args: Vec<OsString>) -> io::Result<()> {
    let code = crate::trampoline::relay_child_and_wait(cached, &args)?;
    std::process::exit(code);
}

/// No execute bit on Windows.
pub fn ensure_owner_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}
