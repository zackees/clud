//! Unix-only tests for `crate::win_creation_flags`.

use crate::win_creation_flags::*;

#[test]
fn invisible_helper_flags_is_zero_off_windows() {
    // Spreading `creation_flags(invisible_helper_flags())` into a
    // `std::process::Command` on Linux/macOS must be a true no-op:
    // the bit pattern is zero, so `creation_flags(0)` leaves the
    // `Command` semantically untouched. The `creationflags` field on
    // `ProcessConfig` is `Option<u32>` and we return `None` to hit
    // running-process-core's "no override" short-circuit.
    assert_eq!(invisible_helper_flags(), 0);
    assert!(invisible_helper_creationflags().is_none());
}
