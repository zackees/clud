//! Process stdio redirection (#2386), Windows side. Rust's `eprintln!` calls
//! `GetStdHandle(STD_ERROR_HANDLE)` on every write, so replacing the process
//! handle is enough; no CRT fd is involved.

use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};

/// Point this process's stderr at `file`. A duplicate of the handle is
/// installed and intentionally leaked: the process owns it until exit.
pub fn redirect_stderr_to(file: &File) -> io::Result<()> {
    let dup = file.try_clone()?;
    let handle = HANDLE(dup.as_raw_handle());
    // SAFETY: `handle` is a live file handle; it is leaked below on success.
    unsafe { SetStdHandle(STD_ERROR_HANDLE, handle) }.map_err(io::Error::other)?;
    std::mem::forget(dup);
    Ok(())
}
