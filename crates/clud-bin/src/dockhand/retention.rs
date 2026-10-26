//! Retention tiers and the pressure eviction order (#926 v2 §5, §6).
//!
//! Pure decisions over injected facts: which tier a generation is in, whether
//! age alone makes it eligible, and what count or byte pressure evicts. The
//! executor that reaps and removes comes in a later slice and must recheck
//! leases and ownership immediately before each destruction.

use std::cmp::Reverse;
use std::collections::BTreeMap;

pub const SUPERSEDED_TTL_SECS: u64 = 24 * 60 * 60;
pub const WARM_IDLE_TTL_SECS: u64 = 14 * 24 * 60 * 60;
/// A live session's CWD protects its workspace only while that session has
/// been active this recently (§5, bounded live-CWD protection).
pub const LIVE_CWD_ACTIVITY_SECS: u64 = 24 * 60 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub superseded_ttl_secs: u64,
    pub warm_idle_ttl_secs: u64,
    pub live_cwd_activity_secs: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            superseded_ttl_secs: SUPERSEDED_TTL_SECS,
            warm_idle_ttl_secs: WARM_IDLE_TTL_SECS,
            live_cwd_activity_secs: LIVE_CWD_ACTIVITY_SECS,
        }
    }
}

/// What the registry knows about one generation: the resources of one
/// `(workspace, stack, role, scope digest)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Generation {
    pub repo_id: String,
    pub workspace_id: String,
    pub stack: String,
    pub role: String,
    pub scope_digest: String,
    /// Unix seconds of the last lease release or use.
    pub last_used: u64,
    pub size_bytes: u64,
    /// Referenced by a live lease.
    pub leased: bool,
    /// The newest generation of its `(workspace, stack, role)`.
    pub newest: bool,
    /// The workspace passed a done signal (explicit, or derived and gated).
    pub workspace_done: bool,
    /// Unix seconds of the latest activity of a live clud session whose CWD
    /// is in this workspace, if any.
    pub live_cwd_activity: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Done,
    Superseded,
    Warm,
    Leased,
}

pub fn tier(generation: &Generation) -> Tier {
    if generation.leased {
        Tier::Leased
    } else if generation.workspace_done {
        Tier::Done
    } else if generation.newest {
        Tier::Warm
    } else {
        Tier::Superseded
    }
}

/// Whether a live session's CWD pins this generation's workspace now.
fn cwd_protects(generation: &Generation, now: u64, policy: &Policy) -> bool {
    generation
        .live_cwd_activity
        .is_some_and(|at| now.saturating_sub(at) < policy.live_cwd_activity_secs)
}

/// The age-only decision, with the reason `status` shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Protected(String),
    Kept(String),
    Eligible(String),
}

pub fn age_verdict(generation: &Generation, now: u64, policy: &Policy) -> Verdict {
    let idle = now.saturating_sub(generation.last_used);
    match tier(generation) {
        Tier::Leased => Verdict::Protected("leased".to_string()),
        Tier::Done => Verdict::Eligible("workspace done".to_string()),
        Tier::Superseded if idle >= policy.superseded_ttl_secs => {
            Verdict::Eligible(format!("superseded, idle {}", hours(idle)))
        }
        Tier::Superseded => Verdict::Kept(format!(
            "superseded, idle {} of {}",
            hours(idle),
            hours(policy.superseded_ttl_secs)
        )),
        Tier::Warm if cwd_protects(generation, now, policy) => {
            Verdict::Protected("live session in workspace".to_string())
        }
        Tier::Warm if idle >= policy.warm_idle_ttl_secs => {
            Verdict::Eligible(format!("warm, idle {}", hours(idle)))
        }
        Tier::Warm => Verdict::Kept(format!(
            "warm, idle {} of {}",
            hours(idle),
            hours(policy.warm_idle_ttl_secs)
        )),
    }
}

fn hours(secs: u64) -> String {
    format!("{}h", secs / 3600)
}

/// Indices of `generations` in the single eviction order: done (oldest
/// first), then superseded (oldest first), then warm (longest idle first);
/// ties by size, largest first. Leased generations and warm ones a live
/// session pins never appear.
pub fn eviction_order(generations: &[Generation], now: u64, policy: &Policy) -> Vec<usize> {
    let mut order: Vec<usize> = (0..generations.len())
        .filter(|&index| {
            let generation = &generations[index];
            match tier(generation) {
                Tier::Leased => false,
                Tier::Warm => !cwd_protects(generation, now, policy),
                Tier::Done | Tier::Superseded => true,
            }
        })
        .collect();
    order.sort_by_key(|&index| {
        let generation = &generations[index];
        (
            tier(generation),
            generation.last_used,
            Reverse(generation.size_bytes),
        )
    });
    order
}

/// Count pressure: evict down the order within each `(repo, stack)` until it
/// holds at most `limit` generations.
pub fn relieve_count(
    generations: &[Generation],
    now: u64,
    policy: &Policy,
    limit: usize,
) -> Vec<usize> {
    let mut counts: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for generation in generations {
        *counts
            .entry((generation.repo_id.as_str(), generation.stack.as_str()))
            .or_default() += 1;
    }
    let mut evicted = Vec::new();
    for index in eviction_order(generations, now, policy) {
        let generation = &generations[index];
        let count = counts
            .get_mut(&(generation.repo_id.as_str(), generation.stack.as_str()))
            .expect("every generation was counted");
        if *count > limit {
            *count -= 1;
            evicted.push(index);
        }
    }
    evicted
}

/// The outcome of byte pressure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ByteRelief {
    pub evict: Vec<usize>,
    pub freed_bytes: u64,
    /// Set when the deficit exceeds every evictable byte: GC stops rather
    /// than grinding caches against a signal it cannot move, and advises
    /// compaction, which stays manual (§6, Lesson 4).
    pub compaction_advisory: Option<String>,
}

/// Byte pressure: evict down the order until `deficit_bytes` is freed. When
/// the evictable total cannot cover the deficit, evict nothing and advise.
pub fn relieve_bytes(
    generations: &[Generation],
    now: u64,
    policy: &Policy,
    deficit_bytes: u64,
) -> ByteRelief {
    let order = eviction_order(generations, now, policy);
    let reclaimable: u64 = order.iter().map(|&i| generations[i].size_bytes).sum();
    if deficit_bytes > reclaimable {
        return ByteRelief {
            evict: Vec::new(),
            freed_bytes: 0,
            compaction_advisory: Some(format!(
                "need {deficit_bytes} bytes but only {reclaimable} are reclaimable; \
                 compact the Docker disk (manual, confirmation-gated)"
            )),
        };
    }
    let mut relief = ByteRelief {
        evict: Vec::new(),
        freed_bytes: 0,
        compaction_advisory: None,
    };
    for index in order {
        if relief.freed_bytes >= deficit_bytes {
            break;
        }
        relief.freed_bytes += generations[index].size_bytes;
        relief.evict.push(index);
    }
    relief
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod tests;
