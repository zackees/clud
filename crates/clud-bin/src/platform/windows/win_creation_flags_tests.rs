//! Windows-only tests for `crate::win_creation_flags`.

use crate::win_creation_flags::*;

#[test]
fn create_no_window_value_matches_winapi() {
    // Sanity: must be exactly 0x0800_0000 — that's the documented
    // CREATE_NO_WINDOW bit (see Microsoft `winbase.h`). Anchoring the
    // literal here means a refactor that quietly flips a digit fails
    // CI instead of silently re-popping consoles in production.
    assert_eq!(CREATE_NO_WINDOW, 0x0800_0000);
    assert_eq!(invisible_helper_flags(), 0x0800_0000);
}

#[test]
fn invisible_helper_creationflags_is_some_on_windows() {
    // The Option<u32> form mirrors `ProcessConfig::creationflags`.
    // `Some(CREATE_NO_WINDOW)` is what every daemon-helper spawn site
    // hands to running-process-core.
    assert_eq!(invisible_helper_creationflags(), Some(0x0800_0000));
    // Both helpers must agree on the bit pattern.
    assert_eq!(
        invisible_helper_creationflags(),
        Some(invisible_helper_flags())
    );
}

/// The exported `CREATE_NEW_PROCESS_GROUP` constant on Windows must
/// match the documented Win32 bit pattern. Anchoring the literal here
/// means a typo in a future refactor fails CI rather than silently
/// re-routing console Ctrl+C events into our backend children.
#[test]
fn create_new_process_group_value_matches_winapi() {
    assert_eq!(CREATE_NEW_PROCESS_GROUP, 0x0000_0200);
}

/// CREATE_NO_WINDOW and CREATE_NEW_PROCESS_GROUP are independent bits
/// (0x0800_0000 vs 0x0000_0200) and must remain composable so a
/// daemon-helper that wants both can OR them without losing either.
#[test]
fn create_no_window_and_create_new_process_group_compose() {
    let combined = invisible_helper_flags() | new_process_group_flags();
    assert_eq!(combined & CREATE_NO_WINDOW, CREATE_NO_WINDOW);
    assert_eq!(
        combined & CREATE_NEW_PROCESS_GROUP,
        CREATE_NEW_PROCESS_GROUP
    );
}

#[test]
fn invisible_helper_flags_does_not_collide_with_detached_process() {
    // DETACHED_PROCESS = 0x0000_0008. The trampoline uses this for the
    // self-spawned daemon launch. CREATE_NO_WINDOW is reserved for the
    // *child-process* helper paths; it is mutually meaningful with
    // DETACHED_PROCESS but the bits don't overlap, so anyone composing
    // them in the future won't lose either signal.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    assert_eq!(invisible_helper_flags() & DETACHED_PROCESS, 0);
}

#[test]
fn invisible_helper_flags_is_idempotent_under_repeated_or() {
    // OR'ing the same bit twice yields the same value — important
    // because some call sites build `creationflags` incrementally and
    // we don't want a future maintainer worrying about double-applies.
    let once = invisible_helper_flags();
    let twice = once | invisible_helper_flags();
    assert_eq!(once, twice);
}
