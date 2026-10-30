//! Process stdio redirection (#2386), Windows side. Rust's `eprintln!` calls
//! `GetStdHandle(STD_ERROR_HANDLE)` on every write, so replacing the process
//! handle is enough; no CRT fd is involved.

use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};

/// The handle a previous redirect installed (0 = none), closed when replaced.
static INSTALLED: AtomicIsize = AtomicIsize::new(0);

/// Point this process's stderr at `file`. A duplicate of the handle is
/// installed and owned by the process until it is replaced by a later
/// redirect (which closes it) or the process exits.
pub fn redirect_stderr_to(file: &File) -> io::Result<()> {
    let new = install_stderr(file)?;
    let old = INSTALLED.swap(new.0 as isize, Ordering::SeqCst);
    if old != 0 {
        // SAFETY: `old` is the duplicate this module installed earlier and
        // stderr no longer refers to it.
        let _ = unsafe { CloseHandle(HANDLE(old as *mut _)) };
    }
    Ok(())
}

/// Install a duplicate of `file`'s handle as `STD_ERROR_HANDLE` and return it.
fn install_stderr(file: &File) -> io::Result<HANDLE> {
    let dup = file.try_clone()?;
    let handle = HANDLE(dup.as_raw_handle());
    // SAFETY: `handle` is a live file handle; it is leaked below on success.
    unsafe { SetStdHandle(STD_ERROR_HANDLE, handle) }.map_err(io::Error::other)?;
    std::mem::forget(dup);
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Console::GetStdHandle;

    #[test]
    fn install_stderr_swaps_the_std_handle_and_restores() {
        let tmp = tempfile::tempdir().unwrap();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(tmp.path().join("err.log"))
            .unwrap();
        // SAFETY: plain query of the process's standard handle.
        let original = unsafe { GetStdHandle(STD_ERROR_HANDLE) }.unwrap_or_default();
        let installed = install_stderr(&file).unwrap();
        // SAFETY: as above.
        let current = unsafe { GetStdHandle(STD_ERROR_HANDLE) }.unwrap();
        // Restore before asserting so a failure cannot leave stderr redirected.
        // SAFETY: `original` was the handle in place before; `installed` is
        // our duplicate and is no longer referenced once restored.
        unsafe {
            SetStdHandle(STD_ERROR_HANDLE, original).unwrap();
            CloseHandle(installed).unwrap();
        }
        assert_eq!(current, installed);
        assert_ne!(installed, original);
        // SAFETY: as above.
        assert_eq!(
            unsafe { GetStdHandle(STD_ERROR_HANDLE) }.unwrap_or_default(),
            original
        );
    }
}
