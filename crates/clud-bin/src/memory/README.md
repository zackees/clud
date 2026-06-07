# memory/

Agent-memory storage layer: SQLite + sqlite-vec for content + KNN,
tantivy for BM25 lexical, and RRF over the two. Pure storage; tier
lifecycle, embeddings, MCP server, daemon IPC, and CLI verbs all live
in sibling sub-issues under META #255 — this directory only owns the
durable on-disk layer and the hybrid-search math.

See [`docs/architecture/memory.md`](../../../../docs/architecture/memory.md)
for the cross-cutting subsystem sketch and
[DD-013](../../../../docs/DESIGN_DECISIONS.md#dd-013-rusqlite-and-redb-coexist-in-clud-bin)
for the rationale on rusqlite + redb coexistence.

## Files

- `mod.rs` — public surface; re-exports `SqliteStore`, `LexicalIndex`,
  `HybridSearchConfig`, `MemoryRow`, `Tier`, `MemoryId`, `MemoryError`,
  `KnnHit`, `LexicalHit`, `FusedHit`, `rrf_fuse`.
- `error.rs:3` `MemoryError` — thiserror enum; `Tantivy(String)` is
  intentionally stringified because some tantivy variants are not
  `Send + 'static`.
- `ids.rs:7` `MemoryId` — uuidv7 newtype; `new_v7()` mints a new id.
- `paths.rs:5` `memory_dir`, `memory_db_path`, `tantivy_dir` — all
  compose off `daemon::default_state_dir`.
- `schema.rs:5` `TARGET_USER_VERSION`, `schema.rs:11` `migrate` — runs
  the embedded `schema/memory_v1.sql` (with `{embed_dim}` interpolated)
  inside one `BEGIN IMMEDIATE`, then sets `PRAGMA user_version = 1`.
  Reopens with a different `embed_dim` raise `MemoryError::DimMismatch`.
- `store.rs:60` `SqliteStore` — the only SQL writer. `open` registers
  sqlite-vec via process-wide `sqlite3_auto_extension`, then opens the
  per-process connection with WAL + foreign_keys + synchronous=NORMAL.
  `insert` writes `memories` and `memory_vec` in one transaction so
  the kill-mid-tx invariant holds; `delete` is symmetric.
- `lexical.rs:48` `LexicalIndex` — tantivy 0.22 BM25 index over the
  schema `(id, session_id, tier, content)`. `upsert` does
  delete-by-term-then-add; `commit()` is explicit and reloads the
  reader.
- `search.rs:6` `HybridSearchConfig` — knobs from `CLUD_MEMORY_RRF_K`
  (default 60) and `CLUD_MEMORY_MAX_RESULTS` (default 50).
  `search.rs:51` `rrf_fuse` — reciprocal-rank-fusion of BM25 + vec
  hits, sorted desc by score with stable insertion-order ties.

## Embedded schema

`crates/clud-bin/schema/memory_v1.sql` is `include_str!`'d by
`schema.rs`. `{embed_dim}` is the only template variable; everything
else is literal SQL. A small sidecar `memory_meta(key, value)` table
pins the dim chosen at first open so subsequent opens can detect a
mismatch without parsing the `vec0` virtual-table DDL.

## Cross-process

Out of scope for this directory; daemon-integration sub-issue wires
`Mutex<SqliteStore>` lifetime inside `__daemon` and exposes
`checkpoint_truncate` to the GC tick.
