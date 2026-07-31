use std::collections::HashSet;
use std::sync::{Arc, Mutex};

#[cfg(not(windows))]
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use crate::process_identity::ProcessIdentity;

#[derive(Clone, Default)]
pub(super) struct ClientLeaseRegistry {
    identities: Arc<Mutex<HashSet<ProcessIdentity>>>,
}

impl ClientLeaseRegistry {
    pub(super) fn acquire(&self, identity: ProcessIdentity) -> Result<usize, &'static str> {
        if !identity.has_start_time() {
            return Err("client lease requires a PID start time");
        }
        if !identity.is_live() {
            return Err("client process identity is not live");
        }
        let mut identities = self.identities.lock().expect("client leases poisoned");
        identities.insert(identity);
        Ok(identities.len())
    }

    pub(super) fn release(&self, identity: ProcessIdentity) -> usize {
        let mut identities = self.identities.lock().expect("client leases poisoned");
        identities.remove(&identity);
        identities.len()
    }

    pub(super) fn snapshot(&self) -> Vec<ProcessIdentity> {
        self.identities
            .lock()
            .expect("client leases poisoned")
            .iter()
            .copied()
            .collect()
    }

    pub(super) fn len(&self) -> usize {
        self.identities
            .lock()
            .expect("client leases poisoned")
            .len()
    }

    pub(super) fn prune_dead(&self) -> Vec<ProcessIdentity> {
        #[cfg(windows)]
        {
            // Direct process handles avoid the Windows process-table refresh
            // that can block in WMI long enough to stall the daemon listener.
            // Probe outside the registry lock so concurrent acquire/release
            // calls remain responsive.
            let dead = self
                .snapshot()
                .into_iter()
                .filter(|identity| !identity.is_live())
                .collect::<HashSet<_>>();
            self.prune_with(|identity| !dead.contains(identity))
        }

        #[cfg(not(windows))]
        {
            self.prune_dead_batched()
        }
    }

    #[cfg(not(windows))]
    fn prune_dead_batched(&self) -> Vec<ProcessIdentity> {
        let identities = self.snapshot();
        if identities.is_empty() {
            return Vec::new();
        }
        let pids: Vec<Pid> = identities
            .iter()
            .map(|identity| Pid::from_u32(identity.pid))
            .collect();
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&pids),
            true,
            ProcessRefreshKind::nothing(),
        );
        self.prune_with(|identity| {
            ProcessIdentity::observe_in(&system, identity.pid)
                .is_some_and(|observed| identity.matches(&observed))
        })
    }

    fn prune_with(&self, is_live: impl Fn(&ProcessIdentity) -> bool) -> Vec<ProcessIdentity> {
        let mut identities = self.identities.lock().expect("client leases poisoned");
        let mut removed = Vec::new();
        identities.retain(|identity| {
            let keep = is_live(identity);
            if !keep {
                removed.push(*identity);
            }
            keep
        });
        removed
    }

    #[cfg(test)]
    fn acquire_for_test(&self, identity: ProcessIdentity) -> usize {
        let mut identities = self.identities.lock().expect("client leases poisoned");
        identities.insert(identity);
        identities.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_acquire_and_release_are_idempotent() {
        let leases = ClientLeaseRegistry::default();
        let identity = ProcessIdentity::new(10, 100);
        assert_eq!(leases.acquire_for_test(identity), 1);
        assert_eq!(leases.acquire_for_test(identity), 1);
        assert_eq!(leases.release(identity), 0);
        assert_eq!(leases.release(identity), 0);
    }

    #[test]
    fn dead_identities_are_pruned() {
        let leases = ClientLeaseRegistry::default();
        let live = ProcessIdentity::new(10, 100);
        let dead = ProcessIdentity::new(20, 200);
        leases.acquire_for_test(live);
        leases.acquire_for_test(dead);

        assert_eq!(leases.prune_with(|identity| *identity == live), vec![dead]);
        assert_eq!(leases.snapshot(), vec![live]);
    }

    #[test]
    fn pid_reuse_mismatch_cannot_release_another_lease() {
        let leases = ClientLeaseRegistry::default();
        let owner = ProcessIdentity::new(10, 100);
        leases.acquire_for_test(owner);

        assert_eq!(leases.release(ProcessIdentity::new(10, 101)), 1);
        assert_eq!(leases.snapshot(), vec![owner]);
    }
}
