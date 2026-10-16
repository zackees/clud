use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System};

use crate::process_identity::ProcessIdentity;

/// Minimal sysinfo refresh: just the parent-PID graph, no CPU/memory/cmdline.
/// `System::new_all()` enumerates every process's full metadata and takes
/// **tens of seconds on Windows** (the same trap that `process_tree::kill_tree`
/// already documents). For the signal helpers and batched sweeps we only
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
    ProcessIdentity::observe(pid).is_some()
}

/// Liveness of a specific process, not merely of a PID.
///
/// `false` both when the PID is gone and when it has been recycled onto an
/// unrelated process. Looks up only `identity.pid`: `ensure_daemon` runs this
/// on every call, and a full process-table refresh here made each reconnect
/// attempt read every `/proc/<pid>/stat` on the machine (#1981).
pub(super) fn identity_is_alive(identity: &ProcessIdentity) -> bool {
    identity.is_live()
}

/// A minimal-refresh `System` snapshot for batched liveness checks.
///
/// A caller reconciling many records refreshes once and reuses this across
/// every [`identity_alive_in`] call, instead of one [`identity_is_alive`]
/// lookup per record.
pub(super) fn refreshed_minimal_system() -> System {
    fresh_minimal_system()
}

/// Batched form of [`identity_is_alive`] against an already-refreshed `System`.
pub(super) fn identity_alive_in(system: &System, identity: &ProcessIdentity) -> bool {
    ProcessIdentity::observe_in(system, identity.pid)
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

/// Descendants of `root`, through the start-time-gated index every clud tree
/// walk shares (`process_tree::children_index`, #1738): a stale parent PID that
/// Windows recycled onto `root` must not pull an older process into the kill.
pub(super) fn descendant_pids(system: &System, root: Pid) -> Vec<Pid> {
    crate::process_tree::descendant_pids(system, root)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// Read syscalls made by the calling thread, which parallel tests do not
    /// disturb.
    fn thread_read_syscalls() -> u64 {
        std::fs::read_to_string("/proc/thread-self/io")
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("syscr: "))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    /// #1981: `ensure_daemon` probes the daemon through `identity_is_alive`, so
    /// it must read only the probed PID's procfs entries, never the whole table.
    #[test]
    fn identity_probe_reads_only_the_probed_pid() {
        let identity = ProcessIdentity::observe(std::process::id()).unwrap();
        let before = thread_read_syscalls();
        assert!(identity_is_alive(&identity));
        assert!(pid_is_alive(identity.pid));
        let reads = thread_read_syscalls() - before;
        // Two lookups plus the counter read itself. A full refresh reads one
        // stat file per process and task on the host.
        assert!(reads <= 32, "liveness probes made {reads} read syscalls");
    }

    #[test]
    fn identity_probe_rejects_a_recycled_pid() {
        let identity = ProcessIdentity::observe(std::process::id()).unwrap();
        let recycled = ProcessIdentity::new(identity.pid, identity.start_time + 1);
        assert!(!identity_is_alive(&recycled));
    }
}
