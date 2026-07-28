//! Which originator PIDs deliberately outlive their `clud` process.
//!
//! Issue #465, safety rule 2. A `--detach` / `--detachable` session moves its
//! children to `Containment::Detached` precisely so they survive the clud that
//! started them. To a sweeper looking for `RUNNING_PROCESS_ORIGINATOR=CLUD:<pid>`
//! with a dead originator, such a subtree is indistinguishable from a leak —
//! the originator really is gone, and the children really are still running.
//!
//! The distinction cannot be recovered from the process table, so it has to be
//! recorded at detach time and **persisted**. A daemon restart that forgot it
//! would reap every detached session on the machine on its first tick, which is
//! a far worse failure than the leak the sweep exists to fix.
//!
//! This module owns the record. It deliberately does not reap anything and
//! knows nothing about killing; see [`crate::orphan_sweep`] for the decision
//! layer that consumes it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Schema version, so a future format change can be detected rather than
/// silently mis-parsed into an empty registry — which would read as
/// "nothing is detached" and re-create the reap-everything failure.
pub const REGISTRY_VERSION: u32 = 1;

/// How long a dead originator's entry is kept before it is eligible for
/// garbage collection (issue #465). Generous on purpose: the cost of keeping
/// a stale entry is that one subtree is never swept, while the cost of
/// dropping one too early is killing a session the user asked to keep.
pub const STALE_ENTRY_TTL_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoverEntry {
    pub originator_pid: u32,
    pub session_id: String,
    pub detached_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoverRegistry {
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<HandoverEntry>,
}

impl Default for HandoverRegistry {
    fn default() -> Self {
        Self {
            version: REGISTRY_VERSION,
            entries: Vec::new(),
        }
    }
}

impl HandoverRegistry {
    /// Originator PIDs whose subtrees must never be swept.
    pub fn protected_pids(&self) -> HashSet<u32> {
        self.entries.iter().map(|e| e.originator_pid).collect()
    }

    /// Record a detach. Re-detaching the same PID replaces the entry rather
    /// than duplicating it — PIDs are reused, so the newest claim wins.
    pub fn insert(&mut self, entry: HandoverEntry) {
        self.entries
            .retain(|e| e.originator_pid != entry.originator_pid);
        self.entries.push(entry);
    }

    pub fn remove_session(&mut self, session_id: &str) {
        self.entries.retain(|e| e.session_id != session_id);
    }

    /// Drop entries whose originator is long dead and whose session is gone.
    ///
    /// Both conditions are required. An entry whose session the daemon still
    /// knows about is live business regardless of age, and an originator that
    /// is still running has not been handed over yet.
    pub fn gc(
        &mut self,
        now_ms: u64,
        originator_is_live: &dyn Fn(u32) -> bool,
        known: &HashSet<String>,
    ) {
        self.entries.retain(|e| {
            if known.contains(&e.session_id) {
                return true;
            }
            if originator_is_live(e.originator_pid) {
                return true;
            }
            now_ms.saturating_sub(e.detached_at_ms) < STALE_ENTRY_TTL_MS
        });
    }
}

pub fn registry_path(state_dir: &Path) -> PathBuf {
    state_dir.join("handover-registry.json")
}

/// Read the registry, or return an empty one when it is absent.
///
/// A malformed or future-versioned file is an **error**, not an empty
/// registry. Defaulting to empty here is the single most dangerous thing this
/// module could do: it would silently un-protect every detached session, and
/// the caller must be able to tell "nothing is detached" from "I could not
/// tell what is detached".
pub fn load(state_dir: &Path) -> std::io::Result<HandoverRegistry> {
    let path = registry_path(state_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HandoverRegistry::default())
        }
        Err(err) => return Err(err),
    };
    let registry: HandoverRegistry = serde_json::from_str(&raw).map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is malformed: {err}", path.display()),
        )
    })?;
    if registry.version != REGISTRY_VERSION {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} has version {}, expected {REGISTRY_VERSION}",
                path.display(),
                registry.version
            ),
        ));
    }
    Ok(registry)
}

/// Write atomically: a torn registry read back after a crash would under-report
/// protected sessions, which is the failure direction that kills things.
pub fn store(state_dir: &Path, registry: &HandoverRegistry) -> std::io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let path = registry_path(state_dir);
    let tmp = path.with_extension(format!("json.tmp{}", std::process::id()));
    let json = serde_json::to_string_pretty(registry)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    std::fs::write(&tmp, json)?;
    // Windows rename-over-existing needs the destination gone first.
    #[cfg(windows)]
    let _ = std::fs::remove_file(&path);
    match std::fs::rename(&tmp, &path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&tmp);
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn entry(pid: u32, session: &str, at_ms: u64) -> HandoverEntry {
        HandoverEntry {
            originator_pid: pid,
            session_id: session.to_string(),
            detached_at_ms: at_ms,
        }
    }

    #[test]
    fn a_missing_registry_reads_as_empty() {
        let tmp = TempDir::new().unwrap();
        let registry = load(tmp.path()).expect("absent file is not an error");
        assert!(registry.entries.is_empty());
        assert_eq!(registry.version, REGISTRY_VERSION);
    }

    #[test]
    fn round_trips_through_disk() {
        let tmp = TempDir::new().unwrap();
        let mut registry = HandoverRegistry::default();
        registry.insert(entry(4321, "sess-a", 1_000));
        registry.insert(entry(9876, "sess-b", 2_000));
        store(tmp.path(), &registry).unwrap();
        assert_eq!(load(tmp.path()).unwrap(), registry);
    }

    #[test]
    fn a_malformed_registry_is_an_error_not_an_empty_one() {
        // The dangerous default. Reading garbage as "nothing is detached"
        // would un-protect every detached session on the machine.
        let tmp = TempDir::new().unwrap();
        std::fs::write(registry_path(tmp.path()), b"{ not json").unwrap();
        let err = load(tmp.path()).expect_err("malformed must not read as empty");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn an_unknown_version_is_an_error_not_an_empty_one() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            registry_path(tmp.path()),
            br#"{"version": 99, "entries": []}"#,
        )
        .unwrap();
        let err = load(tmp.path()).expect_err("future version must not read as empty");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn re_detaching_a_reused_pid_replaces_rather_than_duplicates() {
        // PIDs are reused; two entries for one PID would leave a stale
        // session_id able to keep a subtree protected forever.
        let mut registry = HandoverRegistry::default();
        registry.insert(entry(4321, "old-session", 1_000));
        registry.insert(entry(4321, "new-session", 5_000));
        assert_eq!(registry.entries.len(), 1);
        assert_eq!(registry.entries[0].session_id, "new-session");
    }

    #[test]
    fn gc_keeps_entries_whose_session_is_still_known() {
        let mut registry = HandoverRegistry::default();
        registry.insert(entry(4321, "sess-a", 0));
        let known: HashSet<String> = ["sess-a".to_string()].into_iter().collect();
        // Long past the TTL, and the originator is dead — but the daemon still
        // knows the session, so it is live business.
        registry.gc(STALE_ENTRY_TTL_MS * 10, &|_| false, &known);
        assert_eq!(registry.entries.len(), 1);
    }

    #[test]
    fn gc_keeps_entries_whose_originator_is_still_alive() {
        let mut registry = HandoverRegistry::default();
        registry.insert(entry(4321, "sess-a", 0));
        registry.gc(STALE_ENTRY_TTL_MS * 10, &|_| true, &HashSet::new());
        assert_eq!(registry.entries.len(), 1, "live originator must be kept");
    }

    #[test]
    fn gc_drops_only_long_dead_forgotten_entries() {
        let mut registry = HandoverRegistry::default();
        registry.insert(entry(1, "gone-old", 0));
        registry.insert(entry(2, "gone-recent", STALE_ENTRY_TTL_MS));
        // now = TTL + 1: entry 1 is past the TTL, entry 2 is exactly at it.
        registry.gc(STALE_ENTRY_TTL_MS + 1, &|_| false, &HashSet::new());
        let remaining: Vec<&str> = registry
            .entries
            .iter()
            .map(|e| e.session_id.as_str())
            .collect();
        assert_eq!(remaining, vec!["gone-recent"]);
    }

    #[test]
    fn protected_pids_covers_every_entry() {
        let mut registry = HandoverRegistry::default();
        registry.insert(entry(1, "a", 0));
        registry.insert(entry(2, "b", 0));
        let protected = registry.protected_pids();
        assert!(protected.contains(&1) && protected.contains(&2));
        assert_eq!(protected.len(), 2);
    }
}
