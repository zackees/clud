//! Command-scoped leases (#926 v2 §2).
//!
//! `ensure`, `build`, `run`, `exec` and `shell` hold a lease for the life of
//! the command and heartbeat it. Leases live in the registry, so a daemon
//! restart is not lease loss. These are the pure rules; the caller supplies
//! the clock, the daemon's start time, the last resume from sleep and the
//! result of probing the client process.

/// Heartbeat interval clients use.
pub const HEARTBEAT_SECS: u64 = 30;
/// Default lease TTL.
pub const LEASE_TTL_SECS: u64 = 5 * 60;
/// Floor of the post-start destruction grace window.
pub const MIN_GRACE_SECS: u64 = 10 * 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub id: String,
    pub workspace_id: String,
    pub stack: String,
    /// Resource names the lease protects.
    pub resources: Vec<String>,
    pub client_pid: u32,
    /// The client's process start time, so a reused PID is not mistaken for
    /// the client.
    pub client_start: u64,
    pub acquired_at: u64,
    pub last_heartbeat: u64,
    pub ttl_secs: u64,
}

/// The result of probing a lease's recorded client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientProbe {
    /// The PID is alive and its start time matches.
    Alive,
    /// The PID is gone, or now belongs to a different process.
    Gone,
}

/// Clock facts the daemon knows about itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DaemonClock {
    pub now: u64,
    pub started_at: u64,
    /// When the host last resumed from sleep, if since the daemon started.
    pub resumed_at: Option<u64>,
}

/// After start or resume, no destruction for `max(2 × TTL, 10 min)`, so
/// clients have time to reconnect and resume heartbeats.
pub fn grace_secs(ttl_secs: u64) -> u64 {
    (2 * ttl_secs).max(MIN_GRACE_SECS)
}

/// The end of the current grace window: the later of daemon start and the
/// last resume, plus the grace length.
fn grace_until(clock: &DaemonClock, ttl_secs: u64) -> u64 {
    let since = clock
        .resumed_at
        .map_or(clock.started_at, |at| at.max(clock.started_at));
    since.saturating_add(grace_secs(ttl_secs))
}

/// Whether reap and GC may destroy anything at all right now.
pub fn destruction_allowed(clock: &DaemonClock, ttl_secs: u64) -> bool {
    clock.now >= grace_until(clock, ttl_secs)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeaseState {
    Live,
    /// TTL elapsed but the client is alive, or a grace window is open.
    Held(&'static str),
    Expired,
}

/// A lease expires only when its TTL has elapsed, the client probe fails,
/// and no start or resume grace is open. Two failures, never one.
pub fn lease_state(lease: &Lease, clock: &DaemonClock, probe: ClientProbe) -> LeaseState {
    let stale = clock.now.saturating_sub(lease.last_heartbeat) >= lease.ttl_secs;
    if !stale {
        return LeaseState::Live;
    }
    if probe == ClientProbe::Alive {
        return LeaseState::Held("client alive; heartbeat late");
    }
    if !destruction_allowed(clock, lease.ttl_secs) {
        return LeaseState::Held("start or resume grace");
    }
    LeaseState::Expired
}

/// A container last observed leased at `last_leased` may be force-removed
/// only a full grace window later, so reaping never outruns reconnection.
pub fn reap_allowed(last_leased: u64, clock: &DaemonClock, ttl_secs: u64) -> bool {
    destruction_allowed(clock, ttl_secs)
        && clock.now >= last_leased.saturating_add(grace_secs(ttl_secs))
}

#[cfg(test)]
#[path = "lease_tests.rs"]
mod tests;
