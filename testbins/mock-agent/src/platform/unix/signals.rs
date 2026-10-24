//! Signal dispositions for the mock agent on Unix.

/// Restore the default SIGINT/SIGQUIT dispositions (#1828).
///
/// A non-interactive shell starts a background job with both ignored, and
/// `exec` keeps an ignored disposition, so a test suite run with `&` handed
/// the mock agent an ignored SIGINT: the PTY pump's interrupt then waited out
/// the agent's natural exit. A real agent installs its own handlers.
pub fn restore_default_interrupt() {
    for signal in [libc::SIGINT, libc::SIGQUIT] {
        // SAFETY: setting a disposition to SIG_DFL is always valid.
        unsafe { libc::signal(signal, libc::SIG_DFL) };
    }
}
