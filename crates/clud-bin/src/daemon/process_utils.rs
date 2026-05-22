use std::collections::HashMap;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System};

/// Minimal sysinfo refresh: just the parent-PID graph, no CPU/memory/cmdline.
/// `System::new_all()` enumerates every process's full metadata and takes
/// **tens of seconds on Windows** (the same trap that `process_tree::kill_tree`
/// already documents). For `pid_is_alive` and `signal_process_tree` we only
/// need the PID graph, which the minimal refresh provides in sub-second time
/// — critical on the `clud kill --all` path where a daemon-terminate request
/// calls into these helpers multiple times per session.
fn fresh_minimal_system() -> System {
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    system
}

pub(super) fn pid_is_alive(pid: u32) -> bool {
    let system = fresh_minimal_system();
    system.process(Pid::from_u32(pid)).is_some()
}

pub(super) fn signal_process_tree(root_pid: u32, signal: Signal) {
    let system = fresh_minimal_system();
    let root = Pid::from_u32(root_pid);
    if system.process(root).is_none() {
        return;
    }
    let mut descendants = descendant_pids(&system, root);
    descendants.reverse();
    descendants.push(root);
    for pid in descendants {
        if let Some(process) = system.process(pid) {
            let _ = process.kill_with(signal);
            if matches!(signal, Signal::Kill) {
                let _ = process.kill();
            }
        }
    }
}

pub(super) fn descendant_pids(system: &System, root: Pid) -> Vec<Pid> {
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            children.entry(parent).or_default().push(*pid);
        }
    }
    let mut stack = vec![root];
    let mut descendants = Vec::new();
    while let Some(current) = stack.pop() {
        if let Some(next) = children.get(&current) {
            for child in next {
                descendants.push(*child);
                stack.push(*child);
            }
        }
    }
    descendants
}
