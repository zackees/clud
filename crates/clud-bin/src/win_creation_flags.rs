//! Windows `CREATE_NO_WINDOW` helper for invisible daemon-helper spawns.
//!
//! Background — issue #55: on Windows, when `clud` runs as a parent that
//! spawns daemon helper / worker / repeat-job subprocesses with fully piped
//! stdio, Windows still allocates a console for each child unless we opt
//! out via `CREATE_NO_WINDOW` (`0x0800_0000`). Each allocation is a
//! visible conhost flash that steals focus from the developer's window
//! during the integration test suite (and in production when running
//! `clud --detach` on Windows).
//!
//! This helper is intentionally **opt-in**: only daemon-side spawns whose
//! stdio is fully piped or null should call it. The user-facing
//! `run_plan_subprocess` path in `main.rs` inherits the parent's console
//! (so no new window is created) and must NOT be touched — the user
//! actually wants to see that child's output. The PTY path goes through
//! ConPTY, which manages its own pseudo-console and does not pop a window.
//!
//! On non-Windows platforms the value is `0`, making
//! `creationflags: Some(invisible_helper_flags())` a portable no-op.

/// Bit value of the Windows `CREATE_NO_WINDOW` process creation flag.
///
/// See <https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags>.
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Bit value of the Windows `CREATE_NEW_PROCESS_GROUP` process creation flag.
///
/// The child process becomes the root of a new console process group.
/// CTRL_C_EVENT is masked for the child (it never receives Ctrl+C via the
/// console signal path), but CTRL_BREAK_EVENT can still be delivered with
/// `GenerateConsoleCtrlEvent`. See
/// <https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags>.
#[cfg(windows)]
pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// Returns the creation-flags bitmask to apply to a daemon-helper spawn so
/// that it does not pop a visible console window. On non-Windows, returns
/// `0` (callers can pass it unconditionally).
pub fn invisible_helper_flags() -> u32 {
    #[cfg(windows)]
    {
        CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// `Some(CREATE_NO_WINDOW)` on Windows, `None` elsewhere — the shape that
/// `running_process_core::ProcessConfig::creationflags` expects. Using
/// `None` off-Windows lets the running-process-core priority-flag
/// short-circuit stay intact (passing `Some(0)` would also work, but is
/// semantically misleading).
pub fn invisible_helper_creationflags() -> Option<u32> {
    #[cfg(windows)]
    {
        Some(CREATE_NO_WINDOW)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Returns the creation-flags bitmask that puts the child into its own
/// console process group on Windows. Returns `0` on every other platform
/// so callers can OR it into a flags value unconditionally.
///
/// **Why this matters for the user-facing backend spawn.**
///
/// On Windows, when the user hits Ctrl+C in a clud session, the OS
/// dispatches `CTRL_C_EVENT` to *every process attached to the console
/// process group* — clud itself, the cmd.exe wrapper, the real backend
/// (node.exe for Claude / Codex), and any tool grandchildren. clud's
/// `ctrlc` handler catches it cleanly and prints the resume hint, but the
/// `nodejs-wheel` distribution of Node.js used by some Claude Code
/// installs is a Python launcher that calls `subprocess.communicate()` in
/// blocking mode. Its `WaitForSingleObject` raises `KeyboardInterrupt` in
/// Python, dumping a confusing traceback alongside clud's clean message.
///
/// Placing the child in `CREATE_NEW_PROCESS_GROUP` makes the OS skip the
/// child (and its descendants) when delivering the console Ctrl+C signal,
/// so only clud sees the event. clud is then responsible for tearing the
/// child tree down — which it already does via `process_tree::kill_tree`
/// in the interrupt branch.
pub fn new_process_group_flags() -> u32 {
    #[cfg(windows)]
    {
        CREATE_NEW_PROCESS_GROUP
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// `Some(creationflags)` for the user-facing backend spawn (the child
/// clud actually drives interactively): inherits stdio so the user sees
/// the agent's output, and is placed in a new process group so the OS
/// does not deliver console Ctrl+C events to the child or its descendants.
/// clud's own handler stays in charge of teardown.
///
/// Returns `None` off Windows so running-process-core's `creationflags`
/// short-circuit stays intact — POSIX has no equivalent flag and the
/// terminal foreground-process-group behavior is already correct.
pub fn user_facing_backend_creationflags() -> Option<u32> {
    #[cfg(windows)]
    {
        Some(CREATE_NEW_PROCESS_GROUP)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn create_no_window_value_matches_winapi() {
        // Sanity: must be exactly 0x0800_0000 — that's the documented
        // CREATE_NO_WINDOW bit (see Microsoft `winbase.h`). Anchoring the
        // literal here means a refactor that quietly flips a digit fails
        // CI instead of silently re-popping consoles in production.
        assert_eq!(CREATE_NO_WINDOW, 0x0800_0000);
        assert_eq!(invisible_helper_flags(), 0x0800_0000);
    }

    #[cfg(not(windows))]
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

    #[cfg(windows)]
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

    #[test]
    fn invisible_helper_flags_does_not_collide_with_create_new_process_group() {
        // CREATE_NEW_PROCESS_GROUP = 0x0000_0200. The two flags are
        // independent and can be OR'd together — daemon helpers that
        // need both (none today, but the Python test harness's Ctrl+Break
        // tests do) must compose correctly without losing either bit.
        let create_new_process_group: u32 = 0x0000_0200;
        let combined = invisible_helper_flags() | create_new_process_group;
        assert_eq!(
            combined & create_new_process_group,
            create_new_process_group
        );
        #[cfg(windows)]
        assert_eq!(combined & CREATE_NO_WINDOW, CREATE_NO_WINDOW);
    }

    /// The exported `CREATE_NEW_PROCESS_GROUP` constant on Windows must
    /// match the documented Win32 bit pattern. Anchoring the literal here
    /// means a typo in a future refactor fails CI rather than silently
    /// re-routing console Ctrl+C events into our backend children.
    #[cfg(windows)]
    #[test]
    fn create_new_process_group_value_matches_winapi() {
        assert_eq!(CREATE_NEW_PROCESS_GROUP, 0x0000_0200);
    }

    /// `new_process_group_flags()` returns the Win32 bit pattern on
    /// Windows and `0` elsewhere so callers can `flags |
    /// new_process_group_flags()` unconditionally.
    #[test]
    fn new_process_group_flags_per_os() {
        #[cfg(windows)]
        assert_eq!(new_process_group_flags(), 0x0000_0200);
        #[cfg(not(windows))]
        assert_eq!(new_process_group_flags(), 0);
    }

    /// The `Option<u32>`-shaped helper for `ProcessConfig::creationflags`:
    /// `Some(CREATE_NEW_PROCESS_GROUP)` on Windows, `None` elsewhere so
    /// running-process-core's "no override" short-circuit stays intact on
    /// POSIX where the flag has no meaning.
    #[test]
    fn user_facing_backend_creationflags_per_os() {
        #[cfg(windows)]
        assert_eq!(user_facing_backend_creationflags(), Some(0x0000_0200));
        #[cfg(not(windows))]
        assert!(user_facing_backend_creationflags().is_none());
    }

    /// CREATE_NO_WINDOW and CREATE_NEW_PROCESS_GROUP are independent bits
    /// (0x0800_0000 vs 0x0000_0200) and must remain composable so a
    /// daemon-helper that wants both can OR them without losing either.
    #[cfg(windows)]
    #[test]
    fn create_no_window_and_create_new_process_group_compose() {
        let combined = invisible_helper_flags() | new_process_group_flags();
        assert_eq!(combined & CREATE_NO_WINDOW, CREATE_NO_WINDOW);
        assert_eq!(
            combined & CREATE_NEW_PROCESS_GROUP,
            CREATE_NEW_PROCESS_GROUP
        );
    }

    #[cfg(windows)]
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

    #[cfg(windows)]
    #[test]
    fn invisible_helper_flags_is_idempotent_under_repeated_or() {
        // OR'ing the same bit twice yields the same value — important
        // because some call sites build `creationflags` incrementally and
        // we don't want a future maintainer worrying about double-applies.
        let once = invisible_helper_flags();
        let twice = once | invisible_helper_flags();
        assert_eq!(once, twice);
    }
}
