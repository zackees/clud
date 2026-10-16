//! Unix half of `crate::console_title`: POSIX terminals are out of scope
//! for the title-setting half, so both entry points are no-ops.

/// Never spawns: returns `false`.
pub fn spawn_keeper_thread() -> bool {
    // Title management is out of scope on POSIX (matches the per-call
    // `set_title` no-op). Don't spawn a thread that does nothing.
    false
}

pub fn set_title(_title: &str) {
    // Out of scope per the issue; intentional no-op.
}
