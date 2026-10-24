//! Signal dispositions for the mock agent on Windows.

/// No-op: Windows has no inherited signal dispositions to reset (#1828).
pub fn restore_default_interrupt() {}
