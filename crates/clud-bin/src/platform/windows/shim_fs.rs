//! Windows half of the shim installers' file-mode checks. Windows has no
//! execute bit: every regular file counts as runnable.

use std::fs;
use std::io;
use std::path::Path;

/// Always true: Windows has no execute permission bit.
pub fn has_exec_bit(_metadata: &fs::Metadata) -> bool {
    true
}

/// No-op: Windows has no POSIX permission bits.
pub fn set_mode<P: AsRef<Path>>(_path: P, _mode: u32) -> io::Result<()> {
    Ok(())
}

/// Always `None`: Windows has no POSIX permission bits.
pub fn mode_bits(_metadata: &fs::Metadata) -> Option<u32> {
    None
}
