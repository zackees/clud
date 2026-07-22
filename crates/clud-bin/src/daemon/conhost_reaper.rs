//! Issue #539: bounded reaper for orphaned `conhost.exe` processes.
//!
//! ## Background
//!
//! clud's own daemon-helper spawns already use `CREATE_NO_WINDOW`
//! (`win_creation_flags.rs`, issue #55) and contribute ~0 conhosts. The
//! accumulation observed in #539 (51 live `conhost.exe` processes across 8
//! concurrent `clud --codex` / `clud --claude` sessions) comes from codex
//! (node) spawning tool subprocesses via `child_process` with piped stdio
//! and no `CREATE_NO_WINDOW` — each gets its own `conhost.exe`. Because an
//! interactive codex session runs in inherited-console subprocess mode
//! (`backend.rs`), codex fully owns that spawning and clud never sees it,
//! so clud cannot suppress the conhost at the source. When the tool
//! subprocess dies, its `conhost.exe` can linger.
//!
//! ## Orphan definition
//!
//! A `conhost.exe` is orphaned when its parent process is DEAD:
//!
//! - the parent PID is absent from the current process snapshot entirely
//!   (the parent exited and nothing has reused its PID yet), or
//! - a process now holds that PID but its creation time is LATER than the
//!   conhost's (the PID was reused after the true parent exited — the
//!   PID-reuse guard).
//!
//! If the parent is present and its creation time is at or before the
//! conhost's, the conhost is presumed live and is never touched.
//!
//! ## Safety
//!
//! - Only processes named exactly `conhost.exe` (case-insensitive) are
//!   ever candidates — `OpenConsole.exe` (Windows Terminal's console host)
//!   and everything else is untouched by construction.
//! - Bounded: at most [`MAX_REAP_PER_SWEEP`] terminations per sweep, so one
//!   burst of tool-process exits can't turn into an unbounded kill storm.
//! - [`select_orphaned_conhosts`] is a pure function over a process
//!   snapshot and is exhaustively unit-tested with mock data below — no
//!   real `conhost.exe` (or any other real process) is ever touched by the
//!   test suite.
//!
//! ## Platform notes
//!
//! This module is built on `sysinfo`, which is already cross-platform, so
//! there is no `#[cfg(windows)]` split: [`sweep_once`] takes a fast,
//! syscall-free exit on non-Windows (`conhost.exe` is a Windows-only
//! concept) rather than duplicating the module behind a compile-time
//! `cfg`. This keeps [`select_orphaned_conhosts`] reachable from a normal
//! (non-test) build on every platform, avoiding a dead-code trap under
//! this repo's `cargo clippy --workspace --all-targets -D warnings` gate
//! while still being a true no-op off Windows.
//!
//! The daemon's periodic sweep (`server.rs::spawn_conhost_reap_sweeper`)
//! calls [`sweep_once`] every [`SWEEP_INTERVAL`] and logs activity via
//! `daemon_events::log_event` (`conhost_reap_finished`), matching the
//! issue's "reaper activity is logged/observable via the daemon"
//! acceptance criterion.

use std::collections::HashMap;
use std::time::Duration;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System};

/// One process observed in a snapshot — only the fields the decision
/// logic needs. `start_time` mirrors `sysinfo::Process::start_time()`
/// (seconds since the UNIX epoch in production); only the relative
/// ordering between a conhost and whatever process now holds its recorded
/// parent PID matters, so tests are free to use any consistent unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProcRecord {
    pub(super) pid: u32,
    pub(super) ppid: Option<u32>,
    pub(super) name: String,
    pub(super) start_time: u64,
}

const CONHOST_EXE: &str = "conhost.exe";

/// Bound on terminations per sweep — caps the blast radius of one sweep
/// even if a large batch of tool processes exited at once.
pub(super) const MAX_REAP_PER_SWEEP: usize = 16;

/// How often the daemon's periodic sweep runs. Conservative: orphaned
/// conhosts are cheap to leave alive for up to a minute, and a full
/// process-table enumeration every sweep should stay infrequent.
pub(super) const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Decision function: given a full snapshot of processes on the host,
/// return the PIDs of `conhost.exe` processes that are safe to reap.
///
/// Pure and side-effect free. [`sweep_once`] pairs this with real process
/// enumeration and termination; tests below exercise this function
/// directly with synthetic snapshots so no real process is ever touched.
///
/// `records` must be the full process snapshot (not just conhosts) so
/// parent-liveness lookups can resolve. `max_reap` bounds the number of
/// PIDs returned; iteration follows `records` order, so which candidates
/// get dropped once the bound is hit is deterministic for a given input.
pub(super) fn select_orphaned_conhosts(records: &[ProcRecord], max_reap: usize) -> Vec<u32> {
    if max_reap == 0 {
        return Vec::new();
    }

    // Index every process by pid so "who now holds the conhost's recorded
    // parent pid" is an O(1) lookup — needed for the PID-reuse guard.
    let by_pid: HashMap<u32, &ProcRecord> = records.iter().map(|r| (r.pid, r)).collect();

    let mut victims = Vec::new();
    for record in records {
        if victims.len() >= max_reap {
            break;
        }
        if !record.name.eq_ignore_ascii_case(CONHOST_EXE) {
            continue; // only conhost.exe is ever a candidate (never OpenConsole.exe etc.)
        }
        let Some(ppid) = record.ppid else {
            continue; // no parent info in the snapshot -> never touch
        };
        if ppid == record.pid {
            continue; // degenerate/corrupt self-parented shape -> never touch
        }
        match by_pid.get(&ppid) {
            // Recorded parent pid is absent from the current snapshot
            // entirely: the parent exited and nothing has reused the pid.
            None => victims.push(record.pid),
            // A process now holds that pid. If it started strictly AFTER
            // the conhost, the original parent died and the pid was
            // recycled — the conhost is orphaned under the PID-reuse
            // guard. Equal or earlier start times mean this is genuinely
            // the conhost's live parent, so it is left alone.
            Some(holder) if holder.start_time > record.start_time => victims.push(record.pid),
            Some(_) => {}
        }
    }
    victims
}

/// Report handed back after one sweep, and logged by the caller.
#[derive(Debug, Clone, Default)]
pub(super) struct SweepReport {
    pub(super) scanned_conhosts: usize,
    /// `(conhost pid, dead parent pid)` pairs actually reaped this sweep.
    pub(super) reaped: Vec<(u32, u32)>,
}

/// Enumerate every process on the host into the flat snapshot the
/// decision function consumes.
fn snapshot(system: &System) -> Vec<ProcRecord> {
    system
        .processes()
        .iter()
        .map(|(pid, process)| ProcRecord {
            pid: pid.as_u32(),
            ppid: process.parent().map(|p| p.as_u32()),
            name: process.name().to_string_lossy().into_owned(),
            start_time: process.start_time(),
        })
        .collect()
}

/// Real sweep: enumerate, decide, terminate. Never touches anything that
/// isn't exactly `conhost.exe` with a dead parent (see
/// [`select_orphaned_conhosts`]).
///
/// No-op on non-Windows: `conhost.exe` does not exist there, so this skips
/// the (otherwise harmless but pointless) full-process-table scan
/// entirely rather than paying for it every [`SWEEP_INTERVAL`].
pub(super) fn sweep_once() -> SweepReport {
    if !cfg!(windows) {
        return SweepReport::default();
    }

    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let records = snapshot(&system);
    let ppid_by_pid: HashMap<u32, u32> = records
        .iter()
        .filter_map(|r| r.ppid.map(|ppid| (r.pid, ppid)))
        .collect();
    let scanned_conhosts = records
        .iter()
        .filter(|r| r.name.eq_ignore_ascii_case(CONHOST_EXE))
        .count();

    let victims = select_orphaned_conhosts(&records, MAX_REAP_PER_SWEEP);
    let mut reaped = Vec::with_capacity(victims.len());
    for pid in victims {
        let dead_parent_pid = ppid_by_pid.get(&pid).copied().unwrap_or(0);
        if let Some(process) = system.process(Pid::from_u32(pid)) {
            let _ = process.kill_with(Signal::Kill);
        }
        reaped.push((pid, dead_parent_pid));
    }

    SweepReport {
        scanned_conhosts,
        reaped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(pid: u32, ppid: Option<u32>, name: &str, start_time: u64) -> ProcRecord {
        ProcRecord {
            pid,
            ppid,
            name: name.to_string(),
            start_time,
        }
    }

    #[test]
    fn orphaned_conhost_with_dead_parent_is_reaped() {
        // Parent 500 is simply absent from the snapshot -> dead.
        let records = vec![rec(9001, Some(500), "conhost.exe", 100)];
        assert_eq!(select_orphaned_conhosts(&records, 16), vec![9001]);
    }

    #[test]
    fn conhost_with_live_older_parent_is_kept() {
        let records = vec![
            rec(600, None, "node.exe", 50), // parent, started first
            rec(9002, Some(600), "conhost.exe", 300),
        ];
        assert!(select_orphaned_conhosts(&records, 16).is_empty());
    }

    #[test]
    fn pid_reuse_case_is_reaped() {
        // The conhost's recorded parent pid (500) now belongs to a process
        // that started AFTER the conhost — the real parent died and pid
        // 500 was recycled, so the conhost is orphaned.
        let records = vec![
            rec(9003, Some(500), "conhost.exe", 100),
            rec(500, None, "unrelated.exe", 200),
        ];
        assert_eq!(select_orphaned_conhosts(&records, 16), vec![9003]);
    }

    #[test]
    fn open_console_exe_is_never_reaped() {
        // Same orphan shape as the reaped case above, but the process is
        // Windows Terminal's OpenConsole.exe, not conhost.exe.
        let records = vec![rec(9004, Some(500), "OpenConsole.exe", 100)];
        assert!(select_orphaned_conhosts(&records, 16).is_empty());
    }

    #[test]
    fn bound_is_respected() {
        let records = vec![
            rec(9010, Some(1), "conhost.exe", 100),
            rec(9011, Some(2), "conhost.exe", 100),
            rec(9012, Some(3), "conhost.exe", 100),
        ];
        let victims = select_orphaned_conhosts(&records, 2);
        assert_eq!(victims.len(), 2);
        assert_eq!(victims, vec![9010, 9011]);
    }

    #[test]
    fn non_conhost_process_with_dead_parent_is_ignored() {
        let records = vec![rec(9020, Some(999), "notepad.exe", 100)];
        assert!(select_orphaned_conhosts(&records, 16).is_empty());
    }

    #[test]
    fn conhost_name_match_is_case_insensitive() {
        let records = vec![rec(9030, Some(500), "CONHOST.EXE", 100)];
        assert_eq!(select_orphaned_conhosts(&records, 16), vec![9030]);
    }

    #[test]
    fn conhost_with_no_ppid_info_is_never_touched() {
        let records = vec![rec(9040, None, "conhost.exe", 100)];
        assert!(select_orphaned_conhosts(&records, 16).is_empty());
    }

    #[test]
    fn self_parented_conhost_is_never_touched() {
        // Degenerate/corrupt snapshot shape — never treat as orphaned.
        let records = vec![rec(9050, Some(9050), "conhost.exe", 100)];
        assert!(select_orphaned_conhosts(&records, 16).is_empty());
    }

    #[test]
    fn equal_start_times_are_treated_as_alive_parent() {
        // Same-tick creation is not proof of PID reuse; the strict `>`
        // guard means ties favor "leave it alone."
        let records = vec![
            rec(700, None, "node.exe", 100),
            rec(9060, Some(700), "conhost.exe", 100),
        ];
        assert!(select_orphaned_conhosts(&records, 16).is_empty());
    }

    #[test]
    fn zero_bound_reaps_nothing() {
        let records = vec![rec(9070, Some(500), "conhost.exe", 100)];
        assert!(select_orphaned_conhosts(&records, 0).is_empty());
    }

    #[test]
    fn multiple_independent_orphans_all_selected_under_bound() {
        let records = vec![
            rec(9080, Some(1), "conhost.exe", 100),
            rec(9081, Some(2), "conhost.exe", 100),
        ];
        let mut victims = select_orphaned_conhosts(&records, 16);
        victims.sort_unstable();
        assert_eq!(victims, vec![9080, 9081]);
    }

    #[test]
    fn live_session_conhosts_are_never_touched_alongside_orphans() {
        // Mixed snapshot: one legitimate live console plus one orphan.
        // Only the orphan is selected.
        let records = vec![
            rec(600, None, "node.exe", 50),
            rec(9090, Some(600), "conhost.exe", 300), // live, older parent
            rec(9091, Some(9999), "conhost.exe", 100), // parent absent -> dead
        ];
        assert_eq!(select_orphaned_conhosts(&records, 16), vec![9091]);
    }
}
