//! Unix half of `crate::session_registry`: the `kill(pid, 0)` liveness probe.

pub fn os_probe_is_alive(pid: u32) -> bool {
    if pid == 0 {
        // POSIX: kill(0, 0) signals every process in our group — never
        // useful here, and a misleading "alive" answer.
        return false;
    }
    // SAFETY: kill(pid, 0) does not deliver a signal; it only checks
    // whether we *could* signal the target. Returns 0 on success
    // (process exists and we have permission), -1 with errno set
    // otherwise. ESRCH means the PID is dead; EPERM means it's alive
    // but owned by another user — we count that as alive.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    // `last_os_error` reads errno via the OS-portable wrapper that std
    // already maintains; saves us from chasing __errno_location vs
    // __error link-name shims for every libc flavor.
    let errno = std::io::Error::last_os_error().raw_os_error();
    errno != Some(libc::ESRCH)
}
