//! The dockhand registry: the single-writer record of generations and
//! leases (#926 v2 §1, §2).
//!
//! One JSON file under the daemon's state directory. It carries a persistent
//! `registry_id` stamped on every resource, so resources from another
//! registry are recognised as foreign. A missing file starts a fresh
//! registry; an unreadable one fails closed, because rebuilding it would
//! forget every lease. Writes go to a temporary file and are renamed into
//! place. No Docker I/O happens here.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::lease::Lease;
use super::retention::Generation;

pub const SCHEMA: u32 = 1;
pub const FILE_NAME: &str = "dockhand-registry.json";

/// The persistent facts of one generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationRecord {
    pub repo_id: String,
    pub workspace_id: String,
    pub stack: String,
    pub role: String,
    pub scope_digest: String,
    /// The Docker resource name, from `identity::resource_name`.
    pub name: String,
    pub created_at: u64,
    pub last_used: u64,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    pub schema: u32,
    pub registry_id: String,
    pub generations: Vec<GenerationRecord>,
    pub leases: Vec<Lease>,
}

pub fn path_in(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE_NAME)
}

/// Read the registry. A missing file yields a fresh registry with a new id;
/// an unreadable or newer-schema file is an error, never a silent reset.
pub fn load(path: &Path) -> io::Result<Registry> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return fresh(),
        Err(error) => return Err(error),
    };
    let registry: Registry = serde_json::from_str(&text).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is unreadable: {error}", path.display()),
        )
    })?;
    if registry.schema > SCHEMA {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} has schema {} but this clud reads {SCHEMA}; upgrade clud",
                path.display(),
                registry.schema
            ),
        ));
    }
    Ok(registry)
}

fn fresh() -> io::Result<Registry> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
    let registry_id = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(Registry {
        schema: SCHEMA,
        registry_id,
        generations: Vec::new(),
        leases: Vec::new(),
    })
}

pub fn save(path: &Path, registry: &Registry) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(registry)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

impl Registry {
    /// Record a generation, or refresh its last use and size if known.
    pub fn record_use(&mut self, record: GenerationRecord) {
        match self
            .generations
            .iter_mut()
            .find(|existing| existing.name == record.name)
        {
            Some(existing) => {
                existing.last_used = existing.last_used.max(record.last_used);
                existing.size_bytes = record.size_bytes;
            }
            None => self.generations.push(record),
        }
    }

    /// Add a lease, or replace one with the same id.
    pub fn acquire(&mut self, lease: Lease) {
        self.leases.retain(|existing| existing.id != lease.id);
        self.leases.push(lease);
    }

    /// Returns whether the lease exists.
    pub fn heartbeat(&mut self, lease_id: &str, now: u64) -> bool {
        match self.leases.iter_mut().find(|lease| lease.id == lease_id) {
            Some(lease) => {
                lease.last_heartbeat = lease.last_heartbeat.max(now);
                true
            }
            None => false,
        }
    }

    /// Drop a lease; the generations it protected count as used now, so the
    /// retention clock starts at release.
    pub fn release(&mut self, lease_id: &str, now: u64) -> bool {
        let Some(index) = self.leases.iter().position(|lease| lease.id == lease_id) else {
            return false;
        };
        let lease = self.leases.remove(index);
        for generation in &mut self.generations {
            if lease.resources.contains(&generation.name) {
                generation.last_used = generation.last_used.max(now);
            }
        }
        true
    }

    /// Forget generations whose resources are gone.
    pub fn forget(&mut self, names: &[String]) {
        self.generations
            .retain(|generation| !names.contains(&generation.name));
    }

    /// The retention view: which generations are leased and which is the
    /// newest per `(workspace, stack, role)`. Done state and live-CWD
    /// activity come from the caller, keyed by workspace id.
    pub fn view(
        &self,
        done: &dyn Fn(&str) -> bool,
        live_cwd_activity: &dyn Fn(&str) -> Option<u64>,
    ) -> Vec<Generation> {
        let mut newest: BTreeMap<(&str, &str, &str), (u64, &str)> = BTreeMap::new();
        for record in &self.generations {
            let key = (
                record.workspace_id.as_str(),
                record.stack.as_str(),
                record.role.as_str(),
            );
            let candidate = (record.created_at, record.name.as_str());
            let slot = newest.entry(key).or_insert(candidate);
            if candidate > *slot {
                *slot = candidate;
            }
        }
        self.generations
            .iter()
            .map(|record| Generation {
                repo_id: record.repo_id.clone(),
                workspace_id: record.workspace_id.clone(),
                stack: record.stack.clone(),
                role: record.role.clone(),
                scope_digest: record.scope_digest.clone(),
                last_used: record.last_used,
                size_bytes: record.size_bytes,
                leased: self
                    .leases
                    .iter()
                    .any(|lease| lease.resources.contains(&record.name)),
                newest: newest
                    .get(&(
                        record.workspace_id.as_str(),
                        record.stack.as_str(),
                        record.role.as_str(),
                    ))
                    .is_some_and(|(_, name)| *name == record.name),
                workspace_done: done(&record.workspace_id),
                live_cwd_activity: live_cwd_activity(&record.workspace_id),
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
