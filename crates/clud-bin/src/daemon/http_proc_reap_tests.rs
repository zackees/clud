use super::*;
use crate::daemon::types::{ProcRow, ProcTier};

fn row(pid: u32, start_time: u64, live: bool) -> ProcRow {
    ProcRow {
        pid,
        ppid: Some(1),
        originator: "CLUD:1".to_string(),
        originator_pid: Some(1),
        session_id: None,
        session_name: None,
        cpu_pct: 0.0,
        cpu_ewma_pct: 0.0,
        rss_bytes: 0,
        age_secs: 0,
        start_time,
        command: "child".to_string(),
        depth: 1,
        tier: ProcTier::Cold,
        live,
        exited_at_ms: None,
    }
}

fn snapshot(rows: Vec<ProcRow>) -> ProcTreeSnapshot {
    let mut snapshot = ProcTreeSnapshot::empty(1_000);
    snapshot.sampler_pid = 4_000_000;
    snapshot.rows = rows;
    snapshot
}

#[test]
fn a_live_sampled_identity_may_be_reaped() {
    let snap = snapshot(vec![row(42, 1_700, true)]);
    assert_eq!(authorize(&snap, ProcessIdentity::new(42, 1_700)), Ok(()));
}

#[test]
fn a_recycled_pid_an_exited_row_or_an_untracked_pid_is_refused() {
    let snap = snapshot(vec![row(42, 1_700, true), row(43, 1_800, false)]);
    for identity in [
        ProcessIdentity::new(42, 1_701),
        ProcessIdentity::new(43, 1_800),
        ProcessIdentity::new(44, 1_900),
    ] {
        assert_eq!(
            authorize(&snap, identity),
            Err("process is not a live sampled clud process"),
            "{identity:?}"
        );
    }
}

#[test]
fn an_unknown_start_time_or_the_daemon_itself_is_refused() {
    let snap = snapshot(vec![row(4_000_000, 1_700, true), row(42, 0, true)]);
    assert_eq!(
        authorize(&snap, ProcessIdentity::new(42, UNKNOWN_START_TIME)),
        Err("start_time required")
    );
    assert_eq!(
        authorize(&snap, ProcessIdentity::new(4_000_000, 1_700)),
        Err("refusing to reap the daemon")
    );
}
