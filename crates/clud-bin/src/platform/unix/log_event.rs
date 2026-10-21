//! Unix half of `crate::log_event`.

/// Parent PID of the current process: `getppid()`.
pub fn parent_pid() -> u32 {
    // SAFETY: `getppid` is always-safe libc.
    unsafe { libc::getppid() as u32 }
}
