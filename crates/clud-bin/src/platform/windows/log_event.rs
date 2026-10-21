//! Windows half of `crate::log_event`.

/// Parent PID of the current process: walk the process snapshot for the
/// entry matching our own PID and read its parent.
pub fn parent_pid() -> u32 {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let me = std::process::id();
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[Pid::from_u32(me)]),
        true,
        ProcessRefreshKind::nothing(),
    );
    sys.process(Pid::from_u32(me))
        .and_then(|p| p.parent())
        .map(|p| p.as_u32())
        .unwrap_or(0)
}
