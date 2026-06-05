# memory/embedder/

Embedder abstraction for the agent-memory subsystem (issue #257). Three
kinds: `Local` (fastembed/ort, MiniLM-L6-v2, 384-dim), `Remote` (HTTP to
Anthropic/OpenAI/Gemini/Ollama), and `Disabled` (returns
`MemoryError::EmbedderDisabled` with the four-path remediation message).

See [`docs/architecture/memory.md`](../../../../../docs/architecture/memory.md#embedder)
for the cross-cutting subsystem sketch and
[DD-014](../../../../../docs/DESIGN_DECISIONS.md#dd-014-local-embedder-via-fastembed--windows-arm-carve-out)
for why `LocalEmbedder` is gated behind `memory_local_embed` + a non-Windows-ARM
target stanza.

## Files

- `mod.rs` — public surface; `EmbedderTrait`, `Embedder` enum,
  `embedder_from_env`, `reembed_all` library primitive,
  `EMBED_DIM_MINILM_L6_V2 = 384`, and the env-var constants below.
- `local.rs` — `LocalEmbedder` wrapping `fastembed::TextEmbedding`. Gated
  on `cfg(all(feature = "memory_local_embed", not(all(target_arch =
  "aarch64", target_os = "windows"))))`. First-run downloads the model
  into `<state_dir>/memory/models/` via fastembed's built-in cache;
  progress is surfaced to stderr.
- `remote.rs` — `RemoteEmbedder` HTTP client for the four supported
  providers. Pure blocking `ureq` (no tokio). One trait impl, one body
  shape per provider, normalised parse errors.
- `test_embedder.rs` — `TestEmbedder` for unit tests. Hashes input into
  a deterministic `Vec<f32>` of configurable dim so storage + embed
  contracts can be exercised without the real MiniLM model.

## Public surface

```rust
pub trait EmbedderTrait {
    fn embed(&self, text: &str) -> Result<Vec<f32>, MemoryError>;
    fn dim(&self) -> usize;
    fn name(&self) -> &str;
}

pub enum Embedder { Local(LocalEmbedder), Remote(RemoteEmbedder), Disabled { reason: String } }
pub fn embedder_from_env() -> Result<Embedder, MemoryError>;
pub fn reembed_all<E: EmbedderTrait>(store: &mut SqliteStore, embedder: &E) -> Result<usize, MemoryError>;
```

`reembed_all` is the library primitive the `clud memory reembed` CLI verb
(landing in #262) will wrap with `--resume` checkpointing and the
shadow-table swap for dim drift.

## Environment variables

| Var | Effect |
|---|---|
| `CLUD_MEMORY_EMBEDDER` | `local` (default on non-Windows-ARM) / `remote` / `disabled`. |
| `CLUD_MEMORY_EMBEDDER_PROVIDER` | `anthropic` (= voyage) / `openai` / `gemini` / `ollama`. Setting this implies `CLUD_MEMORY_EMBEDDER=remote`. |
| `CLUD_MEMORY_EMBEDDER_URL` | Override the provider default URL (e.g. self-hosted Ollama). |
| `CLUD_MEMORY_EMBEDDER_API_KEY` | Bearer token for Anthropic/OpenAI, `x-goog-api-key` for Gemini. Unused for Ollama. |
| `CLUD_MEMORY_EMBEDDER_MODEL` | Override the provider default model id. |

## Resolution order in `embedder_from_env`

1. `CLUD_MEMORY_EMBEDDER=disabled` → `Embedder::Disabled` (the four-path
   message lives in `mod.rs::disabled_reason`).
2. `CLUD_MEMORY_EMBEDDER=remote` *or* any of `_PROVIDER` / `_URL` set →
   `Embedder::Remote` via `RemoteEmbedder::from_env`.
3. `cfg(memory_local_embed && not Windows-ARM)` → `Embedder::Local` with
   the default MiniLM-L6-v2 model (downloads on first run).
4. Otherwise → `Embedder::Disabled`.

## Windows-ARM carve-out

`fastembed` pulls in `ort` (ONNX Runtime) which has no prebuilt for
`aarch64-pc-windows-msvc`. Mirrors the `whisper-rs` stanza at
`crates/clud-bin/Cargo.toml:103`:

```toml
[features]
default = ["memory_local_embed"]
memory_local_embed = ["dep:fastembed"]

[target.'cfg(not(all(target_arch = "aarch64", target_os = "windows")))'.dependencies]
fastembed = { version = "4", optional = true }
```

On Windows-ARM, `Embedder::Local` is **not a variant** (the enum literally
doesn't have it on that cfg); `embedder_from_env` falls through to step
4 above and returns `Embedder::Disabled`. CI on `windows-11-arm` runs
`cargo build --no-default-features` to verify.

## Recipe: Ollama on a sibling x86/Linux/macOS box (Windows-ARM workaround)

On the sibling:

```
ollama pull nomic-embed-text
OLLAMA_HOST=0.0.0.0:11434 ollama serve
```

Verify reachable from Windows-ARM:

```
curl http://<host>:11434/api/tags
```

On the Windows-ARM clud machine, set:

```
setx CLUD_MEMORY_EMBEDDER_PROVIDER ollama
setx CLUD_MEMORY_EMBEDDER_URL      http://<host>:11434/api/embeddings
```

Restart the daemon. Memory saves now embed remotely (768-dim), so the
storage layer's `embed_dim` must match — a future `clud memory init` /
`clud memory reembed` will handle the dim swap; for v0.1 of #257 the dim
mismatch surfaces as `MemoryError::DimMismatch` at insert time.

## Testing

Default `cargo test`:

- `memory::embedder::remote::tests::*` — provider parsing, response
  shapes, mock-client round-trip.
- `memory::embedder::test_embedder::tests::*` — deterministic
  hash-based embedder (used by `reembed_all` test below).
- `memory::embedder::tests::*` — `Disabled` error path, env
  dispatch, `reembed_all_replaces_vectors_in_place`,
  `reembed_all_rejects_dim_mismatch`.

`#[ignore]`'d (manual smoke):

- `memory::embedder::local::tests::local_embedder_produces_384_dim_vectors`
- `memory::embedder::local::tests::local_embedder_two_distinct_texts_have_cosine_similarity_below_1`
- `memory::embedder::tests::embedder_from_env_local_default_when_no_env_set`

Run with:

```
soldr cargo test -p clud --lib memory::embedder::local::tests:: -- --ignored --nocapture
```

These download the MiniLM model (~80 MB) on first run; not part of CI.
