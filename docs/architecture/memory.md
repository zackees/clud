# Agent memory

Cross-cutting sketch of the agent-memory subsystem. Source of truth for
the storage layer; sibling sub-issues under META #255 will fill in the
embedder, MCP server, daemon-IPC routes, retention/decay policy,
knowledge-graph, and CLI verbs.

## Status

PR1 (this commit) ships the storage + hybrid-search foundation only:

- `crates/clud-bin/src/memory/` — SqliteStore, LexicalIndex, RRF fusion,
  embedded `memory_v1.sql` migration runner.
- No daemon-IPC routes, no MCP server, no CLI verbs, no embedder.

See [`crates/clud-bin/src/memory/README.md`](../../crates/clud-bin/src/memory/README.md)
for the file-by-file map and
[DD-013](../DESIGN_DECISIONS.md#dd-013-rusqlite-and-redb-coexist-in-clud-bin)
for why this subsystem reintroduces rusqlite alongside redb.

## Architecture sketch

Three on-disk artifacts under `~/.clud/state/memory/`:

- `memory.db` — SQLite (WAL) holding the typed schema:
  `memories`, `sessions`, `memory_relations`, `lessons`, `actions`,
  plus the `vec0` virtual table `memory_vec` (sqlite-vec) for KNN.
  Migrations are gated by `PRAGMA user_version`; the embedding
  dimension is baked into `memory_vec` at first open and pinned in
  `memory_meta` so subsequent opens with a different dim error rather
  than silently rebuild.
- `memory.db-wal` / `memory.db-shm` — SQLite WAL companions; the
  daemon checkpoints with `PRAGMA wal_checkpoint(TRUNCATE)` on its gc
  tick (wired in a later sub-issue).
- `tantivy/` — tantivy 0.22 index directory for the BM25 lexical side.

A save touches three structures inside one critical section:

1. Caller holds the `Mutex<SqliteStore>`.
2. `SqliteStore::insert` runs `BEGIN IMMEDIATE` → INSERT `memories`
   → INSERT `memory_vec` (vec_f32 blob) → COMMIT, all on the same
   connection. Partial failure rolls both inserts back.
3. Caller drops the SQLite mutex, then calls
   `LexicalIndex::upsert` + `commit()`. If the tantivy commit crashes
   between (2) and (3), the row is durable in SQLite and KNN-searchable
   but absent from BM25 until a reconciliation pass runs. The
   reconciliation pass is the testing sub-issue's deliverable.

Read paths (BM25 and KNN) are independent; the hybrid query path runs
both, fuses ranks with reciprocal rank fusion
([`memory::search::rrf_fuse`](../../crates/clud-bin/src/memory/search.rs))
keyed on `MemoryId`, and returns the top
`min(k, CLUD_MEMORY_MAX_RESULTS)` results sorted desc by RRF score.

## Sibling sub-issues (META #255)

- Embeddings (local + remote + Windows-ARM strategy)
- Tier lifecycle (Working / Episodic / Semantic + decay + promotion)
- MCP server in daemon + `clud mcp` stdio bridge
- Daemon HTTP/IPC routes for memory ops
- `clud memory search` / `save` CLI verbs
- Cross-process persistence test
- Knowledge graph (deferred past v1)

Each sub-issue lands on top of the public surface this PR exposes from
[`memory/mod.rs`](../../crates/clud-bin/src/memory/mod.rs); none of
them need to touch the storage layer itself.
