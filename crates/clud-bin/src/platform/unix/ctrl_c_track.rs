//! Unix half of `crate::ctrl_c_track`.

use crate::ctrl_c_track::CtrlCForensics;

/// The forensics snapshot is Windows-only (console process lists and the
/// foreground window have no POSIX equivalent).
pub fn platform_forensics(_child_root_pid: Option<u32>) -> Option<CtrlCForensics> {
    None
}

#[cfg(test)]
mod tests {
    use crate::ctrl_c_track::{double_tap_enabled, test_state_lock};

    /// The guard is Windows-only by design — non-Windows users have
    /// always exited on the first SIGINT and the issue explicitly says
    /// non-Windows behavior should stay unchanged.
    #[test]
    fn double_tap_enabled_is_false_on_non_windows() {
        let _guard = test_state_lock().lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            !double_tap_enabled(),
            "non-Windows must keep single-press semantics"
        );
    }
}
