//! In-process [`LocalEmbedder`] using fastembed (ONNX Runtime).
//!
//! Default model: MiniLM-L6-v2 (384-dim), cached under
//! `<state_dir>/memory/models/`. Gated on the `memory_local_embed` Cargo
//! feature and the non-Windows-ARM target carve-out.

use std::path::PathBuf;
use std::sync::Mutex;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

use crate::memory::embedder::{EmbedderTrait, EMBED_DIM_MINILM_L6_V2};
use crate::memory::error::MemoryError;
use crate::memory::paths::memory_dir;

pub struct LocalEmbedder {
    inner: Mutex<TextEmbedding>,
    dim: usize,
    name: String,
}

impl LocalEmbedder {
    /// Load the default model (MiniLM-L6-v2). First-run downloads cache
    /// the ONNX file under `<state_dir>/memory/models/`. Surfaces
    /// download progress to stderr via fastembed's `show_download_progress`.
    pub fn load_default() -> Result<Self, MemoryError> {
        let cache_dir = default_model_cache_dir()
            .map_err(|e| MemoryError::EmbedderModelLoad(format!("resolve cache dir: {e}")))?;
        std::fs::create_dir_all(&cache_dir)?;

        let init = InitOptions::new(EmbeddingModel::AllMiniLML6V2)
            .with_cache_dir(cache_dir)
            .with_show_download_progress(true);

        let model = TextEmbedding::try_new(init)
            .map_err(|e| MemoryError::EmbedderModelLoad(format!("fastembed init: {e}")))?;

        Ok(Self {
            inner: Mutex::new(model),
            dim: EMBED_DIM_MINILM_L6_V2,
            name: "fastembed/all-MiniLM-L6-v2".to_string(),
        })
    }
}

impl EmbedderTrait for LocalEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, MemoryError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| MemoryError::EmbedderModelLoad(format!("lock poisoned: {e}")))?;
        let mut batch = guard
            .embed(vec![text.to_string()], None)
            .map_err(|e| MemoryError::EmbedderModelLoad(format!("fastembed embed: {e}")))?;
        let v = batch.pop().ok_or_else(|| {
            MemoryError::EmbedderModelLoad("fastembed returned empty batch".to_string())
        })?;
        if v.len() != self.dim {
            return Err(MemoryError::DimMismatch {
                expected: self.dim,
                got: v.len(),
            });
        }
        Ok(v)
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn name(&self) -> &str {
        &self.name
    }
}

fn default_model_cache_dir() -> std::io::Result<PathBuf> {
    Ok(memory_dir()?.join("models"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The two #[ignore]'d tests below exercise the real fastembed +
    // MiniLM-L6-v2 model load. They download ~80 MB on first run and
    // take seconds to load even with the model cached. Skipped in default
    // `cargo test` so CI doesn't pay the model-download cost on every
    // matrix job. The deterministic `TestEmbedder` (see
    // `test_embedder.rs`) covers the equivalent dim + similarity contract
    // for normal CI runs.
    //
    // Manual smoke: `soldr cargo test -p clud --lib \
    //   memory::embedder::local::tests:: -- --ignored --nocapture`.

    #[test]
    #[ignore = "downloads MiniLM model on first run; manual smoke only"]
    fn local_embedder_produces_384_dim_vectors() {
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: this test runs serially via `cargo test -- --ignored`
        // and mutates the documented `CLUD_DAEMON_STATE_DIR` env var.
        unsafe {
            std::env::set_var("CLUD_DAEMON_STATE_DIR", tmp.path());
        }
        let e = LocalEmbedder::load_default().expect("local embedder");
        let v = e.embed("hello").expect("embed");
        assert_eq!(v.len(), 384);
        assert_eq!(e.dim(), 384);
        unsafe {
            std::env::remove_var("CLUD_DAEMON_STATE_DIR");
        }
    }

    #[test]
    #[ignore = "downloads MiniLM model on first run; manual smoke only"]
    fn local_embedder_two_distinct_texts_have_cosine_similarity_below_1() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("CLUD_DAEMON_STATE_DIR", tmp.path());
        }
        let e = LocalEmbedder::load_default().expect("local embedder");
        let a = e.embed("hello").expect("embed a");
        let b = e.embed("totally different topic").expect("embed b");
        let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        let cos = dot / (na * nb);
        assert!(
            cos < 0.999,
            "expected cos<0.999 for unrelated texts, got {cos}"
        );
        unsafe {
            std::env::remove_var("CLUD_DAEMON_STATE_DIR");
        }
    }
}
