//! Unix half of `crate::trampoline::relay_child_and_wait`.
//!
//! Raw `std::process::Command` is deliberate here, exactly as in the neutral
//! `trampoline.rs` (see its `relay_child_and_wait` docs, #333); both files
//! share the `trampoline.rs` exemption in `ci/banned_imports.py`.

/// Run the prepared relay `command` and return its exit code.
pub fn relay_status(command: &mut std::process::Command) -> std::io::Result<i32> {
    let status = command.status()?;
    // A signalled child yields `None`; 1 is the conventional failed
    // stand-in. Production Unix hopping uses `execv`, but keeping this
    // helper total also makes its direct integration coverage portable.
    Ok(status.code().unwrap_or(1))
}
