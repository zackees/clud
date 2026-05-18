use std::collections::HashMap;

use sysinfo::{Pid, Signal, System};

pub(super) fn pid_is_alive(pid: u32) -> bool {
    let system = System::new_all();
    system.process(Pid::from_u32(pid)).is_some()
}

pub(super) fn signal_process_tree(root_pid: u32, signal: Signal) {
    let system = System::new_all();
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
