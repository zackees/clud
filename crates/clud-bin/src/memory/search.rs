use std::collections::HashMap;

use crate::memory::ids::MemoryId;
use crate::memory::lexical::LexicalHit;
use crate::memory::store::KnnHit;

const DEFAULT_RRF_K: u32 = 60;
const DEFAULT_MAX_RESULTS: usize = 50;
const ENV_RRF_K: &str = "CLUD_MEMORY_RRF_K";
const ENV_MAX_RESULTS: &str = "CLUD_MEMORY_MAX_RESULTS";

#[derive(Debug, Clone, Copy)]
pub struct HybridSearchConfig {
    pub rrf_k: u32,
    pub max_results: usize,
}

impl Default for HybridSearchConfig {
    fn default() -> Self {
        Self {
            rrf_k: DEFAULT_RRF_K,
            max_results: DEFAULT_MAX_RESULTS,
        }
    }
}

impl HybridSearchConfig {
    pub fn from_env() -> Self {
        let rrf_k = std::env::var(ENV_RRF_K)
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_RRF_K);
        let max_results = std::env::var(ENV_MAX_RESULTS)
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_MAX_RESULTS);
        Self { rrf_k, max_results }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FusedHit {
    pub id: MemoryId,
    pub score: f32,
    pub bm25_rank: Option<u32>,
    pub vec_rank: Option<u32>,
}

pub fn rrf_fuse(bm25: &[LexicalHit], vec: &[KnnHit], cfg: &HybridSearchConfig) -> Vec<FusedHit> {
    let mut by_id: HashMap<MemoryId, FusedHit> = HashMap::new();
    let mut order: Vec<MemoryId> = Vec::new();

    let k = cfg.rrf_k as f32;

    for (rank0, hit) in bm25.iter().enumerate() {
        let rank = (rank0 + 1) as f32;
        let contrib = 1.0 / (k + rank);
        let entry = by_id.entry(hit.id.clone()).or_insert_with(|| {
            order.push(hit.id.clone());
            FusedHit {
                id: hit.id.clone(),
                score: 0.0,
                bm25_rank: None,
                vec_rank: None,
            }
        });
        entry.score += contrib;
        entry.bm25_rank = Some((rank0 + 1) as u32);
    }

    for (rank0, hit) in vec.iter().enumerate() {
        let rank = (rank0 + 1) as f32;
        let contrib = 1.0 / (k + rank);
        let entry = by_id.entry(hit.id.clone()).or_insert_with(|| {
            order.push(hit.id.clone());
            FusedHit {
                id: hit.id.clone(),
                score: 0.0,
                bm25_rank: None,
                vec_rank: None,
            }
        });
        entry.score += contrib;
        entry.vec_rank = Some((rank0 + 1) as u32);
    }

    // Sort by score desc; stable ties preserve insertion order (BM25 first,
    // then vec) which gives a reproducible total order even when two ids tie.
    let mut fused: Vec<FusedHit> = order
        .into_iter()
        .map(|id| by_id.remove(&id).unwrap())
        .collect();
    fused.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    fused.truncate(cfg.max_results);
    fused
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::lexical::LexicalHit;
    use crate::memory::store::KnnHit;

    fn id_n(n: u8) -> MemoryId {
        // Deterministic uuidv7-shaped string; we only need parseability and
        // distinctness for RRF math, not real time-ordering here.
        let raw = format!("0190{n:02x}00-0000-7000-8000-000000000000");
        MemoryId::parse(&raw).unwrap()
    }

    #[test]
    fn rrf_fuses_overlapping_hits_higher_than_singletons() {
        let shared = id_n(1);
        let bm25_only = id_n(2);
        let vec_only = id_n(3);
        let bm25 = vec![
            LexicalHit {
                id: shared.clone(),
                bm25: 5.0,
            },
            LexicalHit {
                id: bm25_only.clone(),
                bm25: 4.0,
            },
        ];
        let vec = vec![
            KnnHit {
                id: shared.clone(),
                distance: 0.1,
            },
            KnnHit {
                id: vec_only.clone(),
                distance: 0.2,
            },
        ];
        let cfg = HybridSearchConfig::default();
        let fused = rrf_fuse(&bm25, &vec, &cfg);
        let shared_hit = fused.iter().find(|h| h.id == shared).unwrap();
        let bm25_hit = fused.iter().find(|h| h.id == bm25_only).unwrap();
        let vec_hit = fused.iter().find(|h| h.id == vec_only).unwrap();
        assert!(shared_hit.score > bm25_hit.score);
        assert!(shared_hit.score > vec_hit.score);
        assert_eq!(fused.first().unwrap().id, shared);
    }

    #[test]
    fn rrf_respects_max_results_cap() {
        let bm25: Vec<LexicalHit> = (0..20)
            .map(|i| LexicalHit {
                id: id_n(i as u8),
                bm25: 1.0,
            })
            .collect();
        let cfg = HybridSearchConfig {
            rrf_k: 60,
            max_results: 7,
        };
        let fused = rrf_fuse(&bm25, &[], &cfg);
        assert_eq!(fused.len(), 7);
    }

    #[test]
    fn rrf_handles_empty_inputs() {
        let cfg = HybridSearchConfig::default();
        assert!(rrf_fuse(&[], &[], &cfg).is_empty());
    }

    #[test]
    fn rrf_k_from_env_overrides_default() {
        // SAFETY: tests within a single cfg(test) module run on a shared
        // process env; the from_env call below reads the var we just set.
        unsafe {
            std::env::set_var(ENV_RRF_K, "1");
            std::env::set_var(ENV_MAX_RESULTS, "3");
        }
        let cfg = HybridSearchConfig::from_env();
        assert_eq!(cfg.rrf_k, 1);
        assert_eq!(cfg.max_results, 3);
        unsafe {
            std::env::remove_var(ENV_RRF_K);
            std::env::remove_var(ENV_MAX_RESULTS);
        }
    }
}
