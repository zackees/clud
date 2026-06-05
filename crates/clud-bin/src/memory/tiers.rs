//! Tier lifecycle, retention, decay, and auto-forget.
//!
//! Three-tier model:
//!
//! - **Working** — short-lived per-session scratch; auto-forgotten when
//!   `now - last_access_at_ms > working_ttl_ms`.
//! - **Episodic** — session-summarized, manually deleted only.
//! - **Semantic** — durable cross-session knowledge, manually deleted only.
//!
//! Promotion is one-way (Working → Episodic → Semantic) and gated on two
//! signals: an access-count floor and a minimum dwell time since the last
//! tier change. The dwell gate prevents promotion thrash near the
//! threshold.
//!
//! Auto-forget is **scoped to Working only**. Episodic and Semantic
//! retention is a user concern, not a daemon concern; we surface candidates
//! for review via the retention score but never delete those tiers
//! automatically.
//!
//! The consolidation timer / `tick()` driver and Stop-hook entry points
//! live in sibling sub-issues (#261, hooks). This module exposes pure
//! primitives only.
//!
//! ## Environment overrides
//!
//! - `CLUD_MEMORY_WORKING_TTL_MS` — Working-tier TTL in milliseconds
//!   (default `86_400_000` = 24 h).
//! - `CLUD_MEMORY_PROMOTE_ACCESS_FLOOR` — minimum `access_count` to be
//!   eligible for promotion (default `3`).
//! - `CLUD_MEMORY_PROMOTE_DWELL_MS` — minimum dwell since
//!   `tier_change_at_ms` before re-promotion (default `3_600_000` = 1 h).
//! - `CLUD_MEMORY_DECAY_HALF_LIFE_MS` — half-life of the retention-score
//!   recency term in milliseconds (default `604_800_000` = 7 days).

use crate::memory::error::MemoryError;
use crate::memory::ids::MemoryId;
use crate::memory::lexical::LexicalIndex;
use crate::memory::store::{MemoryRow, SqliteStore, Tier};

pub const DEFAULT_WORKING_TTL_MS: u64 = 24 * 60 * 60 * 1_000;
pub const DEFAULT_PROMOTE_ACCESS_FLOOR: u32 = 3;
pub const DEFAULT_PROMOTE_DWELL_MS: u64 = 60 * 60 * 1_000;
pub const DEFAULT_DECAY_HALF_LIFE_MS: u64 = 7 * 24 * 60 * 60 * 1_000;

pub const ENV_WORKING_TTL_MS: &str = "CLUD_MEMORY_WORKING_TTL_MS";
pub const ENV_PROMOTE_ACCESS_FLOOR: &str = "CLUD_MEMORY_PROMOTE_ACCESS_FLOOR";
pub const ENV_PROMOTE_DWELL_MS: &str = "CLUD_MEMORY_PROMOTE_DWELL_MS";
pub const ENV_DECAY_HALF_LIFE_MS: &str = "CLUD_MEMORY_DECAY_HALF_LIFE_MS";

/// All tier-lifecycle knobs in one place.
///
/// Constructed via [`TierConfig::default`] for the documented defaults or
/// [`TierConfig::from_env`] to apply env-var overrides on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierConfig {
    /// Working-tier TTL. Rows with `tier == Working` and
    /// `now_ms - last_access_at_ms > working_ttl_ms` are auto-forgotten.
    pub working_ttl_ms: u64,
    /// Minimum `access_count` for promotion eligibility.
    pub promote_access_floor: u32,
    /// Minimum dwell since `tier_change_at_ms` before promotion fires.
    pub promote_dwell_ms: u64,
    /// Half-life of the recency-decay term in [`retention_score`].
    pub decay_half_life_ms: u64,
    /// Whether the Episodic tier is exportable to git-tracked artifacts.
    /// Working is never exportable; Semantic is always exportable. The
    /// Episodic decision is policy-configurable.
    pub episodic_exportable: bool,
}

impl Default for TierConfig {
    fn default() -> Self {
        Self {
            working_ttl_ms: DEFAULT_WORKING_TTL_MS,
            promote_access_floor: DEFAULT_PROMOTE_ACCESS_FLOOR,
            promote_dwell_ms: DEFAULT_PROMOTE_DWELL_MS,
            decay_half_life_ms: DEFAULT_DECAY_HALF_LIFE_MS,
            episodic_exportable: false,
        }
    }
}

impl TierConfig {
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            working_ttl_ms: env_u64(ENV_WORKING_TTL_MS, d.working_ttl_ms),
            promote_access_floor: env_u32(ENV_PROMOTE_ACCESS_FLOOR, d.promote_access_floor),
            promote_dwell_ms: env_u64(ENV_PROMOTE_DWELL_MS, d.promote_dwell_ms),
            decay_half_life_ms: env_u64(ENV_DECAY_HALF_LIFE_MS, d.decay_half_life_ms).max(1),
            episodic_exportable: d.episodic_exportable,
        }
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(default)
}

/// Returns the rows whose tier should advance under `cfg`, paired with the
/// next tier. Pure read; the caller must apply the promotions via
/// [`apply_promotions`].
///
/// Promotion is Working → Episodic and Episodic → Semantic, with both
/// transitions gated by `access_count >= promote_access_floor` and
/// `now_ms - tier_change_at_ms >= promote_dwell_ms`.
pub fn promote_candidates(
    store: &SqliteStore,
    now_ms: u64,
    cfg: &TierConfig,
) -> Result<Vec<(MemoryId, Tier)>, MemoryError> {
    let mut out = Vec::new();
    for (from, to) in [
        (Tier::Working, Tier::Episodic),
        (Tier::Episodic, Tier::Semantic),
    ] {
        for row in store.list_by_tier(from)? {
            if is_promotion_candidate(&row, now_ms, cfg) {
                out.push((row.id, to));
            }
        }
    }
    Ok(out)
}

fn is_promotion_candidate(row: &MemoryRow, now_ms: u64, cfg: &TierConfig) -> bool {
    if row.access_count < cfg.promote_access_floor {
        return false;
    }
    let dwell = now_ms.saturating_sub(row.tier_change_at_ms);
    dwell >= cfg.promote_dwell_ms
}

/// Apply the promotion list returned by [`promote_candidates`].
///
/// Each promotion updates both the SQLite tier column (via
/// `SqliteStore::promote_tier`) and the lexical index's tier field (via
/// `LexicalIndex::upsert`) so BM25 stays in lockstep with the canonical
/// store. The lexical commit is flushed at the end.
pub fn apply_promotions(
    store: &mut SqliteStore,
    lexical: &mut LexicalIndex,
    promotions: &[(MemoryId, Tier)],
    now_ms: u64,
) -> Result<(), MemoryError> {
    if promotions.is_empty() {
        return Ok(());
    }
    for (id, to) in promotions {
        store.promote_tier(id, *to, now_ms)?;
        let row = store
            .fetch(id)?
            .ok_or_else(|| MemoryError::NotFound(id.clone()))?;
        lexical.upsert(
            &row.id,
            row.session_id.as_deref(),
            row.scope_key.as_deref(),
            row.tier,
            &row.content,
        )?;
    }
    lexical.commit()?;
    Ok(())
}

/// Compute a retention score in `[0.0, 1.0]` blending recency decay, an
/// access-count boost, and a tier floor. The score is used by callers to
/// rank surface candidates; auto-forget is **not** driven by this score
/// (see [`forget_expired`]).
///
/// Formula:
///
/// ```text
/// recency = 0.5 ^ (Δt / half_life)        where Δt = now_ms - last_access_at_ms
/// access  = 1 - 1 / (1 + access_count)
/// floor   = tier_floor(tier)              0.0 Working, 0.25 Episodic, 0.5 Semantic
/// score   = clamp01(floor + (1 - floor) * (0.7 * recency + 0.3 * access))
/// ```
pub fn retention_score(row: &MemoryRow, now_ms: u64, cfg: &TierConfig) -> f32 {
    let half_life = cfg.decay_half_life_ms.max(1) as f64;
    let dt = now_ms.saturating_sub(row.last_access_at_ms) as f64;
    let recency = 0.5_f64.powf(dt / half_life);
    let access = 1.0_f64 - 1.0 / (1.0 + row.access_count as f64);
    let floor = tier_floor(row.tier) as f64;
    let blended = 0.7 * recency + 0.3 * access;
    let score = floor + (1.0 - floor) * blended;
    score.clamp(0.0, 1.0) as f32
}

fn tier_floor(tier: Tier) -> f32 {
    match tier {
        Tier::Working => 0.0,
        Tier::Episodic => 0.25,
        Tier::Semantic => 0.5,
    }
}

/// Delete expired Working-tier rows. Returns the number of rows deleted.
///
/// Spec rule (#258, this PR): Episodic and Semantic are NEVER auto-forgotten.
/// Users opt into long-term storage via promotion; bypassing that is an
/// explicit MCP delete.
pub fn forget_expired(
    store: &mut SqliteStore,
    lexical: &mut LexicalIndex,
    now_ms: u64,
    cfg: &TierConfig,
) -> Result<usize, MemoryError> {
    let working = store.list_by_tier(Tier::Working)?;
    let mut deleted = 0usize;
    for row in working {
        let age = now_ms.saturating_sub(row.last_access_at_ms);
        if age > cfg.working_ttl_ms && store.delete(&row.id)? {
            lexical.delete(&row.id)?;
            deleted += 1;
        }
    }
    if deleted > 0 {
        lexical.commit()?;
    }
    Ok(deleted)
}

/// Whether memories of `tier` should be serialized into git-tracked
/// artifacts under the policy in `cfg`. Hook for #264.
///
/// - Working: never (transient).
/// - Episodic: configurable via `cfg.episodic_exportable`.
/// - Semantic: always.
pub fn tier_exportable(tier: Tier, cfg: &TierConfig) -> bool {
    match tier {
        Tier::Working => false,
        Tier::Episodic => cfg.episodic_exportable,
        Tier::Semantic => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vec384(seed: f32) -> Vec<f32> {
        (0..384).map(|i| seed + i as f32 * 0.001).collect()
    }

    fn fresh_store(tmp: &tempfile::TempDir) -> SqliteStore {
        SqliteStore::open(&tmp.path().join("memory.db"), 384).unwrap()
    }

    fn fresh_lexical(tmp: &tempfile::TempDir) -> LexicalIndex {
        LexicalIndex::open_or_create(&tmp.path().join("tantivy")).unwrap()
    }

    fn make_row(
        tier: Tier,
        access_count: u32,
        last_access_at_ms: u64,
        tier_change_at_ms: u64,
        content: &str,
    ) -> MemoryRow {
        MemoryRow {
            id: MemoryId::new_v7(),
            session_id: None,
            scope_key: None,
            branch_name: None,
            is_orphan: false,
            tier,
            content: content.to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            tier_change_at_ms,
            access_count,
            last_access_at_ms,
            metadata_json: None,
        }
    }

    fn insert(store: &mut SqliteStore, row: &MemoryRow, seed: f32) {
        store.insert(row, &vec384(seed)).unwrap();
    }

    fn insert_lex(lexical: &mut LexicalIndex, row: &MemoryRow) {
        lexical
            .upsert(
                &row.id,
                row.session_id.as_deref(),
                row.scope_key.as_deref(),
                row.tier,
                &row.content,
            )
            .unwrap();
        lexical.commit().unwrap();
    }

    #[test]
    fn promote_candidates_picks_working_to_episodic_above_access_floor() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = fresh_store(&tmp);
        let cfg = TierConfig::default();
        let now_ms = cfg.promote_dwell_ms * 2;
        let above = make_row(Tier::Working, cfg.promote_access_floor, 0, 0, "a");
        let below = make_row(
            Tier::Working,
            cfg.promote_access_floor.saturating_sub(1),
            0,
            0,
            "b",
        );
        insert(&mut store, &above, 0.10);
        insert(&mut store, &below, 0.11);
        let cands = promote_candidates(&store, now_ms, &cfg).unwrap();
        let ids: Vec<MemoryId> = cands.iter().map(|(id, _)| id.clone()).collect();
        assert!(ids.contains(&above.id));
        assert!(!ids.contains(&below.id));
        assert!(cands
            .iter()
            .all(|(_, t)| matches!(t, Tier::Episodic | Tier::Semantic)));
    }

    #[test]
    fn promote_candidates_requires_minimum_dwell_time() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = fresh_store(&tmp);
        let cfg = TierConfig::default();
        // tier_change_at_ms == now_ms means dwell == 0 → not eligible.
        let now_ms = 10_000_000u64;
        let recent = make_row(
            Tier::Working,
            cfg.promote_access_floor + 10,
            now_ms,
            now_ms,
            "recent",
        );
        let old = make_row(
            Tier::Working,
            cfg.promote_access_floor + 10,
            now_ms,
            now_ms - cfg.promote_dwell_ms,
            "old",
        );
        insert(&mut store, &recent, 0.20);
        insert(&mut store, &old, 0.21);
        let cands = promote_candidates(&store, now_ms, &cfg).unwrap();
        let ids: Vec<MemoryId> = cands.into_iter().map(|(id, _)| id).collect();
        assert!(!ids.contains(&recent.id));
        assert!(ids.contains(&old.id));
    }

    #[test]
    fn apply_promotions_updates_tier_and_bm25_in_lockstep() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = fresh_store(&tmp);
        let mut lexical = fresh_lexical(&tmp);
        let cfg = TierConfig::default();
        let now_ms = cfg.promote_dwell_ms * 2;
        let row = make_row(
            Tier::Working,
            cfg.promote_access_floor + 1,
            0,
            0,
            "promote me",
        );
        insert(&mut store, &row, 0.30);
        insert_lex(&mut lexical, &row);

        // Sanity: before promotion, search filtered to Episodic-floor returns
        // nothing because the row is still Working.
        let pre = lexical
            .search("promote", 10, None, Some(Tier::Episodic), None)
            .unwrap();
        assert!(pre.is_empty());

        let cands = promote_candidates(&store, now_ms, &cfg).unwrap();
        apply_promotions(&mut store, &mut lexical, &cands, now_ms).unwrap();

        let fetched = store.fetch(&row.id).unwrap().unwrap();
        assert_eq!(fetched.tier, Tier::Episodic);
        let post = lexical
            .search("promote", 10, None, Some(Tier::Episodic), None)
            .unwrap();
        assert_eq!(post.len(), 1);
        assert_eq!(post[0].id, row.id);
    }

    #[test]
    fn retention_score_decays_with_recency() {
        let cfg = TierConfig::default();
        let now_ms = 365u64 * 24 * 60 * 60 * 1_000;
        let h1 = make_row(
            Tier::Working,
            5,
            now_ms - 60 * 60 * 1_000,
            now_ms - 60 * 60 * 1_000,
            "x",
        );
        let h24 = make_row(
            Tier::Working,
            5,
            now_ms - 24 * 60 * 60 * 1_000,
            now_ms - 24 * 60 * 60 * 1_000,
            "x",
        );
        let d7 = make_row(
            Tier::Working,
            5,
            now_ms - 7 * 24 * 60 * 60 * 1_000,
            now_ms - 7 * 24 * 60 * 60 * 1_000,
            "x",
        );
        let s1 = retention_score(&h1, now_ms, &cfg);
        let s24 = retention_score(&h24, now_ms, &cfg);
        let s7 = retention_score(&d7, now_ms, &cfg);
        assert!(s1 > s24, "1h={s1} should beat 24h={s24}");
        assert!(s24 > s7, "24h={s24} should beat 7d={s7}");
    }

    #[test]
    fn retention_score_floors_by_tier() {
        let cfg = TierConfig::default();
        let now_ms = 1_000_000_000u64;
        let last_access = now_ms - 10 * 60 * 1_000;
        let w = make_row(Tier::Working, 5, last_access, last_access, "x");
        let e = make_row(Tier::Episodic, 5, last_access, last_access, "x");
        let s = make_row(Tier::Semantic, 5, last_access, last_access, "x");
        let sw = retention_score(&w, now_ms, &cfg);
        let se = retention_score(&e, now_ms, &cfg);
        let ss = retention_score(&s, now_ms, &cfg);
        assert!(ss > se, "semantic={ss} should beat episodic={se}");
        assert!(se > sw, "episodic={se} should beat working={sw}");
    }

    #[test]
    fn forget_expired_drops_only_working_tier() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = fresh_store(&tmp);
        let mut lexical = fresh_lexical(&tmp);
        let cfg = TierConfig::default();
        let now_ms = cfg.working_ttl_ms * 10;
        // All three rows are expired by recency, but only Working should die.
        let w = make_row(Tier::Working, 0, 0, 0, "working");
        let e = make_row(Tier::Episodic, 0, 0, 0, "episodic");
        let s = make_row(Tier::Semantic, 0, 0, 0, "semantic");
        insert(&mut store, &w, 0.40);
        insert(&mut store, &e, 0.41);
        insert(&mut store, &s, 0.42);
        insert_lex(&mut lexical, &w);
        insert_lex(&mut lexical, &e);
        insert_lex(&mut lexical, &s);

        let count = forget_expired(&mut store, &mut lexical, now_ms, &cfg).unwrap();
        assert_eq!(count, 1);
        assert!(store.fetch(&w.id).unwrap().is_none());
        assert!(store.fetch(&e.id).unwrap().is_some());
        assert!(store.fetch(&s.id).unwrap().is_some());
    }

    #[test]
    fn forget_expired_returns_deletion_count() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = fresh_store(&tmp);
        let mut lexical = fresh_lexical(&tmp);
        let cfg = TierConfig::default();
        let now_ms = cfg.working_ttl_ms * 5;
        for i in 0..4 {
            let row = make_row(Tier::Working, 0, 0, 0, "stale");
            insert(&mut store, &row, 0.50 + i as f32 * 0.01);
            insert_lex(&mut lexical, &row);
        }
        // One fresh row (last_access at now) should survive.
        let fresh = make_row(Tier::Working, 0, now_ms, 0, "fresh");
        insert(&mut store, &fresh, 0.60);
        insert_lex(&mut lexical, &fresh);

        let n = forget_expired(&mut store, &mut lexical, now_ms, &cfg).unwrap();
        assert_eq!(n, 4);
        assert!(store.fetch(&fresh.id).unwrap().is_some());
    }

    #[test]
    fn tier_exportable_default_policy_semantic_only() {
        let cfg = TierConfig::default();
        assert!(!tier_exportable(Tier::Working, &cfg));
        assert!(!tier_exportable(Tier::Episodic, &cfg));
        assert!(tier_exportable(Tier::Semantic, &cfg));

        let opt_in = TierConfig {
            episodic_exportable: true,
            ..cfg
        };
        assert!(!tier_exportable(Tier::Working, &opt_in));
        assert!(tier_exportable(Tier::Episodic, &opt_in));
        assert!(tier_exportable(Tier::Semantic, &opt_in));
    }

    // The TierConfig env-var keys are global per-process. Serialize this
    // test internally to avoid interleaving with anything else that touches
    // the same keys.
    #[test]
    fn tier_config_from_env_overrides_defaults() {
        // SAFETY: tests in this crate mutate process env in their own scope;
        // we set then immediately remove the four keys this test owns.
        unsafe {
            std::env::set_var(ENV_WORKING_TTL_MS, "1234");
            std::env::set_var(ENV_PROMOTE_ACCESS_FLOOR, "7");
            std::env::set_var(ENV_PROMOTE_DWELL_MS, "5678");
            std::env::set_var(ENV_DECAY_HALF_LIFE_MS, "999_does_not_parse");
        }
        let cfg = TierConfig::from_env();
        assert_eq!(cfg.working_ttl_ms, 1234);
        assert_eq!(cfg.promote_access_floor, 7);
        assert_eq!(cfg.promote_dwell_ms, 5678);
        // Garbage value falls back to default.
        assert_eq!(cfg.decay_half_life_ms, DEFAULT_DECAY_HALF_LIFE_MS);
        unsafe {
            std::env::remove_var(ENV_WORKING_TTL_MS);
            std::env::remove_var(ENV_PROMOTE_ACCESS_FLOOR);
            std::env::remove_var(ENV_PROMOTE_DWELL_MS);
            std::env::remove_var(ENV_DECAY_HALF_LIFE_MS);
        }
    }
}
