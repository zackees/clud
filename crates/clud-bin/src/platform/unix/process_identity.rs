//! Unix half of `crate::process_identity`: a single-PID sysinfo refresh.

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use crate::process_identity::ProcessIdentity;

/// FILETIME creation ticks are a Windows concept; POSIX has none, so this is
/// always `None`. Only Windows callers consult it.
pub fn creation_ticks(_pid: u32) -> Option<u64> {
    None
}

pub fn observe_process(pid: u32) -> Option<ProcessIdentity> {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
        true,
        ProcessRefreshKind::nothing(),
    );
    ProcessIdentity::observe_in(&system, pid)
}
