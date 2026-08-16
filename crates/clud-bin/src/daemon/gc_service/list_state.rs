//! Issue #896: give every `clud gc list` row a *state* and a *reason*.
//!
//! The extern-repo git-dirty guard made GC refuse to reclaim a checkout
//! holding local work. That is correct, but it means such a row sits in the
//! registry forever looking exactly like garbage GC keeps failing to
//! collect. Surfacing why a row is retained is what makes the retained set
//! legible.
//!
//! **`gc list` must never run the extern-repo git probe.** It is a hot
//! client op — the launch path calls into the same registry worker — and
//! that probe costs up to three subprocesses per checkout. So the verdict
//! is *not* recomputed here: the periodic purge tick evaluates every
//! extern-repo entry anyway, and this module caches what it decided.
//!
//! That does not make `List` subprocess-free, and it would be dishonest to
//! imply so: it still calls `collect_live_lock_paths`, which shells out to
//! `git worktree list --porcelain` via the *unbounded* `worktrees::run_git`.
//! That predates this module; #946 covers moving such work off the worker.

use std::collections::HashMap;

use super::extern_repo::{PurgeClass, PurgeDecision};

/// A purge verdict recorded by the tick, for later display by `gc list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CachedDecision {
    pub(super) class: PurgeClass,
    pub(super) reason: &'static str,
    /// When the tick computed this. Surfaced in `--json` so a consumer can
    /// see how stale the verdict is (worst case one tick, default 1 h).
    pub(super) evaluated_unix: i64,
    /// Which tick recorded it. Eviction compares generations rather than
    /// timestamps so a backwards clock step cannot wipe the cache.
    generation: u64,
}

/// Path → last recorded verdict. Owned by the registry worker loop, which
/// is also the thread that runs the periodic tick, so no lock is needed.
#[derive(Debug, Default)]
pub(super) struct SpareReasons {
    entries: HashMap<String, CachedDecision>,
    generation: u64,
}

impl SpareReasons {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Open a new eviction generation. Verdicts recorded after this call
    /// survive the matching [`SpareReasons::evict_stale`].
    pub(super) fn begin_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    pub(super) fn record(&mut self, path: String, decision: PurgeDecision, evaluated_unix: i64) {
        self.entries.insert(
            path,
            CachedDecision {
                class: decision.class,
                reason: decision.reason,
                evaluated_unix,
                generation: self.generation,
            },
        );
    }

    pub(super) fn get(&self, path: &str) -> Option<&CachedDecision> {
        self.entries.get(path)
    }

    /// Drop verdicts not re-recorded in the current generation. Only sound
    /// after a pass that re-evaluated every cacheable row.
    pub(super) fn evict_stale(&mut self) {
        let current = self.generation;
        self.entries
            .retain(|_, cached| cached.generation == current);
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Displayed state of a registry row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EntryState {
    /// Normal: nothing is holding it, GC will take it on a future tick.
    Reclaimable,
    /// Deliberately retained. Healthy, but it will not be auto-reclaimed.
    Pinned,
    /// The on-disk path is gone — a registry row pointing at nothing.
    Dangling,
}

impl EntryState {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Reclaimable => "reclaimable",
            Self::Pinned => "pinned",
            Self::Dangling => "dangling",
        }
    }
}

/// Pure state derivation, so the precedence between "gone", "live" and
/// "pinned by the tick" is unit-testable without a daemon, a filesystem or
/// a git checkout.
///
/// Precedence, most severe first:
///
/// | condition                     | state       | reason                    |
/// |-------------------------------|-------------|---------------------------|
/// | path missing                  | Dangling    | `dangling: path missing`  |
/// | live session holds it         | Pinned      | `spared: live session`    |
/// | tick class `Dangling`         | Dangling    | the tick's own reason     |
/// | tick class `Pinned`           | Pinned      | the tick's own reason     |
/// | tick class `Spared`           | Reclaimable | the tick's own reason     |
/// | tick class `Reclaimable`      | Reclaimable | none                      |
/// | never evaluated               | Reclaimable | none                      |
///
/// Note the `Spared` row: a checkout that is merely too recently touched
/// *will* be reclaimed once it ages in, so it stays an ordinary reclaimable
/// row. Painting it yellow would make most of a working tree yellow and
/// destroy exactly the signal #896 is trying to add — but its reason is
/// still carried, so `--json` can explain the delay.
pub(super) fn derive_state(
    path_exists: bool,
    live_locked: bool,
    cached: Option<&CachedDecision>,
) -> (EntryState, Option<&'static str>) {
    if !path_exists {
        return (EntryState::Dangling, Some("dangling: path missing"));
    }
    if live_locked {
        return (EntryState::Pinned, Some("spared: live session"));
    }
    match cached {
        Some(d) => match d.class {
            PurgeClass::Dangling => (EntryState::Dangling, Some(d.reason)),
            PurgeClass::Pinned => (EntryState::Pinned, Some(d.reason)),
            PurgeClass::Spared => (EntryState::Reclaimable, Some(d.reason)),
            PurgeClass::Reclaimable => (EntryState::Reclaimable, None),
        },
        // Never evaluated (fresh daemon, first tick pending) — a plain row
        // rather than a fabricated verdict.
        None => (EntryState::Reclaimable, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cached(class: PurgeClass, reason: &'static str) -> CachedDecision {
        CachedDecision {
            class,
            reason,
            evaluated_unix: 1_000,
            generation: 0,
        }
    }

    /// Assert state **and** reason, not just state — a pinned row that
    /// cannot say why is the exact problem #896 exists to fix.
    #[test]
    fn state_matrix_covers_every_precedence_step() {
        // Missing path outranks everything, including a live lock.
        assert_eq!(
            derive_state(
                false,
                true,
                Some(&cached(PurgeClass::Pinned, "pinned: whatever"))
            ),
            (EntryState::Dangling, Some("dangling: path missing"))
        );
        // A live session outranks a cached verdict.
        assert_eq!(
            derive_state(
                true,
                true,
                Some(&cached(
                    PurgeClass::Reclaimable,
                    "reclaimable: clean and pushed"
                ))
            ),
            (EntryState::Pinned, Some("spared: live session"))
        );
        // A recorded pin surfaces the tick's own reason verbatim.
        assert_eq!(
            derive_state(
                true,
                false,
                Some(&cached(
                    PurgeClass::Pinned,
                    "pinned: uncommitted or unpushed work"
                ))
            ),
            (
                EntryState::Pinned,
                Some("pinned: uncommitted or unpushed work")
            )
        );
        // A cached `Dangling` verdict stays dangling.
        assert_eq!(
            derive_state(
                true,
                false,
                Some(&cached(PurgeClass::Dangling, "dangling: path missing"))
            ),
            (EntryState::Dangling, Some("dangling: path missing"))
        );
        // Evaluated and reclaimable → plain row, no reason, no color.
        assert_eq!(
            derive_state(
                true,
                false,
                Some(&cached(
                    PurgeClass::Reclaimable,
                    "reclaimable: clean and pushed"
                ))
            ),
            (EntryState::Reclaimable, None)
        );
        // Never evaluated (fresh daemon) → also plain, not a fake verdict.
        assert_eq!(
            derive_state(true, false, None),
            (EntryState::Reclaimable, None)
        );
    }

    /// A checkout that is merely too recently touched **will** be reclaimed
    /// once it ages in, so it must not be painted as pinned. With the 24 h
    /// default that is most of a working tree; collapsing it into `pinned`
    /// turns the list into a yellow wall and destroys the signal #896 adds.
    /// The reason still rides along so `--json` can explain the delay.
    #[test]
    fn a_merely_too_recent_checkout_stays_reclaimable() {
        let (state, reason) = derive_state(
            true,
            false,
            Some(&cached(PurgeClass::Spared, "spared: recently active")),
        );
        assert_eq!(state, EntryState::Reclaimable);
        assert_eq!(reason, Some("spared: recently active"));
    }

    /// Eviction is keyed on a generation counter, not a timestamp, so a
    /// backwards clock step cannot wipe the cache.
    #[test]
    fn eviction_drops_only_entries_missing_from_the_current_generation() {
        let mut cache = SpareReasons::new();
        let pinned = PurgeDecision {
            purge: false,
            reason: "pinned: uncommitted or unpushed work",
            class: PurgeClass::Pinned,
        };

        cache.begin_generation();
        cache.record("/a".to_string(), pinned, 100);
        cache.record("/b".to_string(), pinned, 100);

        // Next tick re-records only /a — /b's row is gone from the registry.
        cache.begin_generation();
        // A wall clock that jumped *backwards* must not matter.
        cache.record("/a".to_string(), pinned, 1);
        cache.evict_stale();

        assert!(cache.get("/a").is_some(), "re-evaluated row must survive");
        assert!(cache.get("/b").is_none(), "stale row must be evicted");
    }

    #[test]
    fn state_names_are_the_documented_json_values() {
        assert_eq!(EntryState::Reclaimable.as_str(), "reclaimable");
        assert_eq!(EntryState::Pinned.as_str(), "pinned");
        assert_eq!(EntryState::Dangling.as_str(), "dangling");
    }
}
