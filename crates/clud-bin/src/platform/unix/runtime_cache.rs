//! Unix half of `crate::runtime_cache`: the cache hop replaces the process
//! image with `execv`.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;

/// `execv` the cached binary with `args`. Returns only on failure.
pub fn reexec_from_cached_binary(cached: &Path, args: Vec<OsString>) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(
        CString::new(cached.as_os_str().as_bytes())
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?,
    );
    for arg in args {
        argv.push(
            CString::new(arg.as_os_str().as_bytes())
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?,
        );
    }
    let mut argv_ptrs = argv.iter().map(|arg| arg.as_ptr()).collect::<Vec<_>>();
    argv_ptrs.push(std::ptr::null());

    unsafe {
        libc::execv(argv[0].as_ptr(), argv_ptrs.as_ptr());
    }
    Err(io::Error::last_os_error())
}

/// Set the owner execute bit on the freshly copied binary.
pub fn ensure_owner_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    // Preserve executable bit; `fs::copy` already copies mode on
    // Unix but be defensive in case `source` came from a tarball
    // extract that stripped it.
    perms.set_mode(perms.mode() | 0o100);
    fs::set_permissions(path, perms)
}
