use std::collections::HashMap;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System};

use crate::process_identity::ProcessIdentity;

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

/// Bare-PID liveness. Prefer [`identity_is_alive`] anywhere the PID came out
/// of persisted state: this answers "is *some* process holding that number",
/// which after PID reuse is not the same question (issue #558).
pub(super) fn pid_is_alive(pid: u32) -> bool {
    let system = fresh_minimal_system();
    system.process(Pid::from_u32(pid)).is_some()
}

/// Liveness of a specific process, not merely of a PID.
///
/// `false` both when the PID is gone and when it has been recycled onto an
/// unrelated process.
pub(super) fn identity_is_alive(identity: &ProcessIdentity) -> bool {
    let system = fresh_minimal_system();
    ProcessIdentity::observe_in(&system, identity.pid)
        .is_some_and(|observed| identity.matches(&observed))
}

/// Identity-guarded [`signal_process_tree`].
///
/// Does nothing at all if the recorded process is gone — including when a
/// different process has since inherited its PID. This is the variant every
/// caller working from a stored PID must use: signalling the wrong tree is
/// unrecoverable in a way that failing to signal is not.
pub(super) fn signal_process_tree_as(identity: &ProcessIdentity, signal: Signal) {
    let system = fresh_minimal_system();
    let live = ProcessIdentity::observe_in(&system, identity.pid)
        .is_some_and(|observed| identity.matches(&observed));
    if !live {
        return;
    }
    signal_tree_in(&system, Pid::from_u32(identity.pid), signal);
}

pub(super) fn signal_process_tree(root_pid: u32, signal: Signal) {
    let system = fresh_minimal_system();
    let root = Pid::from_u32(root_pid);
    if system.process(root).is_none() {
        return;
    }
    signal_tree_in(&system, root, signal);
}

/// Shared tail of both signal helpers: walk the recorded tree leaves-first and
/// signal it. `system` must already be refreshed, and the caller owns whatever
/// liveness or identity check applies.
fn signal_tree_in(system: &System, root: Pid, signal: Signal) {
    let mut descendants = descendant_pids(system, root);
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
