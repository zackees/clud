//! Embedder abstraction for the agent-memory subsystem (issue #257).
//!
//! Three concrete embedder kinds live under this module:
//!
//! - [`Local`] — in-process MiniLM-L6-v2 via fastembed/ort. Gated on the
//!   `memory_local_embed` Cargo feature and the
//!   `cfg(not(all(target_arch = "aarch64", target_os = "windows")))` carve-out
//!   (no ort prebuilt for Windows-ARM today; mirrors the `whisper-rs` stanza
//!   at `crates/clud-bin/Cargo.toml:103`).
//! - [`Remote`] — one of four HTTP providers (Anthropic / OpenAI / Gemini /
//!   Ollama) via `ureq`. No tokio runtime; selected via env vars.
//! - [`Disabled`] — explicit no-op for environments without local or remote
//!   support. `embed()` returns [`MemoryError::EmbedderDisabled`] with the
//!   four-path remediation message documented in `embedder/README.md`.
//!
//! See the module README for env vars, provider URLs, and the Ollama-on-LAN
//! recipe. `clud memory reembed` (the CLI verb) lands in #262; the library
//! primitive [`reembed_all`] is exposed here for it.
//!
//! [`Local`]: Embedder::Local
//! [`Remote`]: Embedder::Remote
//! [`Disabled`]: Embedder::Disabled

use crate::memory::error::MemoryError;
use crate::memory::store::SqliteStore;

mod remote;
#[cfg(test)]
mod test_embedder;

#[cfg(all(
    feature = "memory_local_embed",
    not(all(target_arch = "aarch64", target_os = "windows"))
))]
mod local;

pub use remote::{RemoteEmbedder, RemoteProvider};

#[cfg(all(
    feature = "memory_local_embed",
    not(all(target_arch = "aarch64", target_os = "windows"))
))]
pub use local::LocalEmbedder;

#[cfg(test)]
pub use test_embedder::TestEmbedder;

/// Default embedding width for MiniLM-L6-v2 — the local model the
/// fastembed wrapper produces and the dimension storage will pin at first
/// migration. Remote providers advertise their own dim via `dim()`.
pub const EMBED_DIM_MINILM_L6_V2: usize = 384;

/// Behaviour every embedder kind exposes. `dim` is queried by storage at
/// open time to decide whether the on-disk `vec0` table matches the live
/// embedder.
pub trait EmbedderTrait {
    fn embed(&self, text: &str) -> Result<Vec<f32>, MemoryError>;
    fn dim(&self) -> usize;
    fn name(&self) -> &str;
}

/// Top-level embedder dispatch. Constructed via [`embedder_from_env`] in
/// production; tests build a [`TestEmbedder`] directly.
///
/// `Local` is boxed because `fastembed::TextEmbedding` carries the ONNX
/// session inline; the unboxed variant inflates every `Embedder` value
/// (including the always-cheap `Disabled` case) to >1 KiB. Boxing keeps
/// the enum word-sized and matches clippy's `large_enum_variant` lint.
pub enum Embedder {
    #[cfg(all(
        feature = "memory_local_embed",
        not(all(target_arch = "aarch64", target_os = "windows"))
    ))]
    Local(Box<LocalEmbedder>),
    Remote(RemoteEmbedder),
    Disabled {
        reason: String,
    },
}

impl EmbedderTrait for Embedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, MemoryError> {
        match self {
            #[cfg(all(
                feature = "memory_local_embed",
                not(all(target_arch = "aarch64", target_os = "windows"))
            ))]
            Embedder::Local(e) => e.embed(text),
            Embedder::Remote(e) => e.embed(text),
            Embedder::Disabled { reason } => Err(MemoryError::EmbedderDisabled(reason.clone())),
        }
    }

    fn dim(&self) -> usize {
        match self {
            #[cfg(all(
                feature = "memory_local_embed",
                not(all(target_arch = "aarch64", target_os = "windows"))
            ))]
            Embedder::Local(e) => e.dim(),
            Embedder::Remote(e) => e.dim(),
            Embedder::Disabled { .. } => 0,
        }
    }

    fn name(&self) -> &str {
        match self {
            #[cfg(all(
                feature = "memory_local_embed",
                not(all(target_arch = "aarch64", target_os = "windows"))
            ))]
            Embedder::Local(e) => e.name(),
            Embedder::Remote(e) => e.name(),
            Embedder::Disabled { .. } => "disabled",
        }
    }
}

/// Environment variables read by [`embedder_from_env`]. Listed here so the
/// README and the tests share one source of truth.
pub const ENV_EMBEDDER_KIND: &str = "CLUD_MEMORY_EMBEDDER";
pub const ENV_EMBEDDER_PROVIDER: &str = "CLUD_MEMORY_EMBEDDER_PROVIDER";
pub const ENV_EMBEDDER_URL: &str = "CLUD_MEMORY_EMBEDDER_URL";
pub const ENV_EMBEDDER_API_KEY: &str = "CLUD_MEMORY_EMBEDDER_API_KEY";
pub const ENV_EMBEDDER_MODEL: &str = "CLUD_MEMORY_EMBEDDER_MODEL";

/// Resolve an [`Embedder`] from process env. Resolution order:
///
/// 1. `CLUD_MEMORY_EMBEDDER=disabled` → [`Embedder::Disabled`].
/// 2. `CLUD_MEMORY_EMBEDDER=remote` (or any of the `_PROVIDER` / `_URL` /
///    `_API_KEY` vars set) → [`Embedder::Remote`] using
///    [`RemoteProvider::from_env`].
/// 3. `CLUD_MEMORY_EMBEDDER=local` (or unset) on a target that builds
///    `memory_local_embed` → [`Embedder::Local`] with the default
///    MiniLM-L6-v2 model. Falls back to [`Embedder::Disabled`] (with the
///    four-path message) if the local backend is not compiled in.
pub fn embedder_from_env() -> Result<Embedder, MemoryError> {
    let kind = std::env::var(ENV_EMBEDDER_KIND).ok();
    let kind_lower = kind.as_deref().map(|s| s.to_ascii_lowercase());

    if kind_lower.as_deref() == Some("disabled") {
        return Ok(Embedder::Disabled {
            reason: disabled_reason("CLUD_MEMORY_EMBEDDER=disabled"),
        });
    }

    let want_remote = kind_lower.as_deref() == Some("remote")
        || std::env::var(ENV_EMBEDDER_PROVIDER).is_ok()
        || std::env::var(ENV_EMBEDDER_URL).is_ok();

    if want_remote {
        return RemoteEmbedder::from_env().map(Embedder::Remote);
    }

    #[cfg(all(
        feature = "memory_local_embed",
        not(all(target_arch = "aarch64", target_os = "windows"))
    ))]
    {
        LocalEmbedder::load_default().map(|e| Embedder::Local(Box::new(e)))
    }

    #[cfg(not(all(
        feature = "memory_local_embed",
        not(all(target_arch = "aarch64", target_os = "windows"))
    )))]
    {
        Ok(Embedder::Disabled {
            reason: disabled_reason(
                "local embedder unavailable on this build (Windows-ARM or --no-default-features)",
            ),
        })
    }
}

/// Four-path remediation message returned from a disabled embedder. Kept
/// concise; the verbatim long-form text lives in the README.
fn disabled_reason(prefix: &str) -> String {
    format!(
        "{prefix}. To enable: (1) set CLUD_MEMORY_EMBEDDER_PROVIDER + \
         CLUD_MEMORY_EMBEDDER_API_KEY for a remote provider, \
         (2) set CLUD_MEMORY_EMBEDDER_PROVIDER=ollama + \
         CLUD_MEMORY_EMBEDDER_URL=http://host:11434 for Ollama on LAN, \
         (3) run clud inside WSL2 (Ubuntu) on Windows-ARM, or \
         (4) wait for ort 2.0 stable + an aarch64-windows ONNX Runtime."
    )
}

/// Re-embed every row in `store` using `embedder` and rewrite the vec
/// table in place. Returns the row count processed.
///
/// Library primitive only — the `clud memory reembed --model <new>` CLI
/// verb lands in #262 and will wrap this with `--resume` checkpointing.
/// This implementation does not currently handle dim drift (the vec0
/// table dim is fixed at first migration); a follow-up will add a
/// shadow-table swap when [`Embedder::dim`] differs from
/// `store.embed_dim()`.
// TODO(#262): CLI surface + --resume checkpoint + shadow-swap for dim drift.
pub fn reembed_all<E: EmbedderTrait>(
    store: &mut SqliteStore,
    embedder: &E,
) -> Result<usize, MemoryError> {
    use rusqlite::params;

    if embedder.dim() != store.embed_dim() {
        return Err(MemoryError::DimMismatch {
            expected: store.embed_dim(),
            got: embedder.dim(),
        });
    }

    // Snapshot ids + content first so we don't iterate a cursor while
    // writing through the same connection.
    let rows: Vec<(String, String)> = {
        let conn = store.conn_ref();
        let mut stmt = conn.prepare("SELECT id, content FROM memories ORDER BY id ASC")?;
        let mut out = Vec::new();
        let mut q = stmt.query([])?;
        while let Some(r) = q.next()? {
            let id: String = r.get(0)?;
            let content: String = r.get(1)?;
            out.push((id, content));
        }
        out
    };

    let mut count = 0usize;
    for (id, content) in &rows {
        let embedding = embedder.embed(content)?;
        if embedding.len() != store.embed_dim() {
            return Err(MemoryError::DimMismatch {
                expected: store.embed_dim(),
                got: embedding.len(),
            });
        }
        let blob = embedding_to_blob(&embedding);
        let conn = store.conn_mut();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM memory_vec WHERE id = ?1", params![id.as_str()])?;
        tx.execute(
            "INSERT INTO memory_vec(id, embedding) VALUES (?1, ?2)",
            params![id.as_str(), blob],
        )?;
        tx.commit()?;
        count += 1;
    }
    Ok(count)
}

fn embedding_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::ids::MemoryId;
    use crate::memory::store::{MemoryRow, Tier};

    fn row(id: MemoryId, content: &str) -> MemoryRow {
        MemoryRow {
            id,
            session_id: None,
            scope_key: None,
            branch_name: None,
            is_orphan: false,
            tier: Tier::Working,
            content: content.to_string(),
            created_at_ms: 1,
            updated_at_ms: 1,
            tier_change_at_ms: 1,
            access_count: 0,
            last_access_at_ms: 1,
            metadata_json: None,
        }
    }

    #[test]
    fn embedder_disabled_returns_embedder_disabled_error() {
        let e = Embedder::Disabled {
            reason: "test".to_string(),
        };
        let err = e.embed("hi").unwrap_err();
        assert!(matches!(err, MemoryError::EmbedderDisabled(_)));
    }

    #[cfg(all(
        feature = "memory_local_embed",
        not(all(target_arch = "aarch64", target_os = "windows"))
    ))]
    #[test]
    #[ignore = "downloads MiniLM model on first run; manual smoke only"]
    fn embedder_from_env_local_default_when_no_env_set() {
        let _g1 = EnvGuard::remove(ENV_EMBEDDER_KIND);
        let _g2 = EnvGuard::remove(ENV_EMBEDDER_PROVIDER);
        let _g3 = EnvGuard::remove(ENV_EMBEDDER_URL);
        let tmp = tempfile::tempdir().unwrap();
        let _g4 = EnvGuard::set("CLUD_DAEMON_STATE_DIR", tmp.path().to_str().unwrap());
        let e = embedder_from_env().expect("embedder");
        assert!(matches!(e, Embedder::Local(_)));
        assert_eq!(e.dim(), 384);
    }

    #[test]
    fn embedder_from_env_disabled_when_kind_disabled() {
        let _g = EnvGuard::set(ENV_EMBEDDER_KIND, "disabled");
        let e = embedder_from_env().unwrap();
        assert!(matches!(e, Embedder::Disabled { .. }));
        assert_eq!(e.name(), "disabled");
        assert_eq!(e.dim(), 0);
    }

    #[test]
    fn disabled_reason_lists_four_remediation_paths() {
        let reason = disabled_reason("test");
        assert!(reason.contains("(1)"));
        assert!(reason.contains("(2)"));
        assert!(reason.contains("(3)"));
        assert!(reason.contains("(4)"));
        assert!(reason.contains("WSL2"));
        assert!(reason.contains("Ollama"));
    }

    #[test]
    fn reembed_all_replaces_vectors_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::open(&tmp.path().join("memory.db"), 8).expect("open");
        // Seed with placeholder vectors (all zeros) so we can prove they
        // change after reembed.
        let mut ids = Vec::new();
        for content in ["alpha", "beta", "gamma"] {
            let id = MemoryId::new_v7();
            store
                .insert(&row(id.clone(), content), &[0.0_f32; 8])
                .unwrap();
            ids.push(id);
        }

        let embedder = TestEmbedder::with_dim(8);
        let n = reembed_all(&mut store, &embedder).unwrap();
        assert_eq!(n, 3);

        // After reembed, the vec blobs for each row should match the
        // deterministic TestEmbedder output for the original content.
        for (id, content) in ids.iter().zip(["alpha", "beta", "gamma"]) {
            let expected = embedder.embed(content).unwrap();
            let stored = store.fetch_vec_for_test(id).unwrap();
            assert_eq!(stored, expected, "vec for {content} must be rewritten");
        }
    }

    #[test]
    fn reembed_all_rejects_dim_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::open(&tmp.path().join("memory.db"), 8).expect("open");
        let embedder = TestEmbedder::with_dim(384);
        let err = reembed_all(&mut store, &embedder).unwrap_err();
        assert!(matches!(
            err,
            MemoryError::DimMismatch {
                expected: 8,
                got: 384
            }
        ));
    }

    /// Single-test env-var guard: sets a var on construction, removes it
    /// on drop. Tests in this module aren't parallel-safe wrt env vars
    /// already (see `paths.rs` and `search.rs`); we use the same idiom.
    struct EnvGuard {
        key: &'static str,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            // SAFETY: tests in this crate already touch process env; the
            // module-level convention is that env-mutating tests are not
            // run concurrently via cargo test default. See ids.rs /
            // search.rs for the same pattern.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key }
        }

        #[allow(dead_code)]
        fn remove(key: &'static str) -> Self {
            // SAFETY: see Self::set.
            unsafe {
                std::env::remove_var(key);
            }
            Self { key }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: see Self::set.
            unsafe {
                std::env::remove_var(self.key);
            }
        }
    }
}
