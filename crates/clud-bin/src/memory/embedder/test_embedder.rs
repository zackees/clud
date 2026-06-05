//! Deterministic in-process test embedder. Hashes input text into a
//! fixed-dim `Vec<f32>` so unit tests can exercise the embedder
//! interface — and `reembed_all` — without downloading the MiniLM model.

use crate::memory::embedder::EmbedderTrait;
use crate::memory::error::MemoryError;

pub struct TestEmbedder {
    dim: usize,
    name: String,
}

impl TestEmbedder {
    pub fn with_dim(dim: usize) -> Self {
        Self {
            dim,
            name: format!("test/{dim}d"),
        }
    }
}

impl EmbedderTrait for TestEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, MemoryError> {
        // Splitmix64 starting from a FNV-1a digest of `text`. The PRNG is
        // deterministic per-input, which is exactly what the tests need.
        let mut state: u64 = 0xcbf2_9ce4_8422_2325;
        for b in text.as_bytes() {
            state ^= *b as u64;
            state = state.wrapping_mul(0x100_0000_01b3);
        }
        let mut out = Vec::with_capacity(self.dim);
        for _ in 0..self.dim {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            // Map to [-1, 1) so the vectors have non-trivial variance.
            let scaled = (z as f32 / u64::MAX as f32) * 2.0 - 1.0;
            out.push(scaled);
        }
        Ok(out)
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedder_produces_requested_dim() {
        let e = TestEmbedder::with_dim(384);
        let v = e.embed("hello").unwrap();
        assert_eq!(v.len(), 384);
        assert_eq!(e.dim(), 384);
    }

    #[test]
    fn test_embedder_is_deterministic() {
        let e = TestEmbedder::with_dim(64);
        let a = e.embed("hello").unwrap();
        let b = e.embed("hello").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn test_embedder_two_distinct_texts_have_cosine_similarity_below_1() {
        let e = TestEmbedder::with_dim(128);
        let a = e.embed("hello").unwrap();
        let b = e.embed("world").unwrap();
        let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        let cos = dot / (na * nb);
        assert!(
            cos < 0.999,
            "expected cos<0.999 for different texts, got {cos}"
        );
    }
}
