use super::*;

const START: u64 = 1_000_000;
const AFTER_GRACE: u64 = START + MIN_GRACE_SECS;

fn lease(last_heartbeat: u64) -> Lease {
    Lease {
        id: "l1".to_string(),
        workspace_id: "ws".to_string(),
        stack: "rust".to_string(),
        resources: vec!["dh-clud-rust-target-1".to_string()],
        client_pid: 42,
        client_start: 7,
        acquired_at: START,
        last_heartbeat,
        ttl_secs: LEASE_TTL_SECS,
    }
}

fn clock(now: u64) -> DaemonClock {
    DaemonClock {
        now,
        started_at: START,
        resumed_at: None,
    }
}

#[test]
fn grace_is_twice_the_ttl_with_a_ten_minute_floor() {
    assert_eq!(grace_secs(LEASE_TTL_SECS), MIN_GRACE_SECS);
    assert_eq!(grace_secs(60), MIN_GRACE_SECS);
    assert_eq!(grace_secs(20 * 60), 40 * 60);
}

#[test]
fn a_fresh_heartbeat_is_live() {
    let now = AFTER_GRACE + 100;
    assert_eq!(
        lease_state(&lease(now - HEARTBEAT_SECS), &clock(now), ClientProbe::Gone),
        LeaseState::Live
    );
}

#[test]
fn a_stale_lease_with_a_live_client_is_held() {
    let now = AFTER_GRACE + 10_000;
    assert_eq!(
        lease_state(&lease(START), &clock(now), ClientProbe::Alive),
        LeaseState::Held("client alive; heartbeat late"),
        "a live client that lost its connection keeps its lease"
    );
}

#[test]
fn expiry_needs_both_a_stale_heartbeat_and_a_dead_client() {
    let now = AFTER_GRACE + 10_000;
    assert_eq!(
        lease_state(&lease(START), &clock(now), ClientProbe::Gone),
        LeaseState::Expired,
        "a SIGKILLed client releases within one TTL"
    );
}

#[test]
fn a_daemon_restart_holds_every_lease_through_the_grace_window() {
    let restarted = DaemonClock {
        now: START + 60,
        started_at: START,
        resumed_at: None,
    };
    assert!(!destruction_allowed(&restarted, LEASE_TTL_SECS));
    assert_eq!(
        lease_state(&lease(START - 3600), &restarted, ClientProbe::Gone),
        LeaseState::Held("start or resume grace")
    );
}

#[test]
fn resume_from_sleep_reopens_the_grace_window() {
    let resumed = DaemonClock {
        now: AFTER_GRACE + 5000,
        started_at: START,
        resumed_at: Some(AFTER_GRACE + 4900),
    };
    assert!(!destruction_allowed(&resumed, LEASE_TTL_SECS));
    assert_eq!(
        lease_state(&lease(START), &resumed, ClientProbe::Gone),
        LeaseState::Held("start or resume grace"),
        "a laptop lid-close never turns a live build into a stray"
    );
    let later = DaemonClock {
        now: AFTER_GRACE + 4900 + MIN_GRACE_SECS,
        ..resumed
    };
    assert!(destruction_allowed(&later, LEASE_TTL_SECS));
}

#[test]
fn reaping_waits_a_full_grace_window_after_the_last_lease() {
    let last_leased = AFTER_GRACE + 100;
    assert!(!reap_allowed(
        last_leased,
        &clock(last_leased + MIN_GRACE_SECS - 1),
        LEASE_TTL_SECS
    ));
    assert!(reap_allowed(
        last_leased,
        &clock(last_leased + MIN_GRACE_SECS),
        LEASE_TTL_SECS
    ));
}
