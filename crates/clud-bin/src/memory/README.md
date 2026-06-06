# memory/

Agent-memory storage layer: SQLite + sqlite-vec for content + KNN,
tantivy for BM25 lexical, and RRF over the two. Pure storage; tier
lifecycle, MCP server, daemon IPC, and CLI verbs all live
in sibling sub-issues under META #255 — this directory only owns the
durable on-disk layer, the hybrid-search math, and (as of #257) the
embedder abstraction that produces the dense vectors storage stores.

See [`docs/architecture/memory.md`](../../../../docs/architecture/memory.md)
for the cross-cutting subsystem sketch and
[DD-013](../../../../docs/DESIGN_DECISIONS.md#dd-013-rusqlite-and-redb-coexist-in-clud-bin)
for the rationale on rusqlite + redb coexistence.

## Files

- `mod.rs` — public surface; re-exports `SqliteStore`, `LexicalIndex`,
  `HybridSearchConfig`, `MemoryRow`, `Tier`, `MemoryId`, `MemoryError`,
  `KnnHit`, `LexicalHit`, `FusedHit`, `rrf_fuse`; the identity surface
  `RepoScope`, `resolve_repo_scope`, `normalize_origin_url`,
  `scope_key`, `branch_isolate`, `branch_unisolate`,
  `cross_repo_glob_filter`; and (from `embedder/`) `Embedder`,
  `EmbedderTrait`, `RemoteEmbedder`, `RemoteProvider`,
  `embedder_from_env`, `reembed_all`, `EMBED_DIM_MINILM_L6_V2`.
- `embedder/` — embedder abstraction (`Local` / `Remote` / `Disabled`),
  fastembed wrapper, four-provider HTTP client, deterministic
  `TestEmbedder`. Carved out on Windows-ARM. See
  [`embedder/README.md`](embedder/README.md).
- `error.rs:3` `MemoryError` — thiserror enum; `Tantivy(String)` is
  intentionally stringified because some tantivy variants are not
  `Send + 'static`.
- `ids.rs:7` `MemoryId` — uuidv7 newtype; `new_v7()` mints a new id.
- `identity.rs:24` `RepoScope`, `identity.rs:52` `resolve_repo_scope`,
  `identity.rs:90` `normalize_origin_url`, `identity.rs:127` `scope_key`,
  `identity.rs:143` `branch_isolate` / `identity.rs:155`
  `branch_unisolate`, `identity.rs:166` `cross_repo_glob_filter` —
  see "Identity & scoping" below.
- `paths.rs:5` `memory_dir`, `memory_db_path`, `tantivy_dir` — all
  compose off `daemon::default_state_dir`.
- `schema.rs:5` `TARGET_USER_VERSION`, `schema.rs:13` `migrate` — runs
  the embedded `schema/memory_v1.sql` then `schema/memory_v2.sql` in a
  forward-only chain (v0 → v1 → v2) each step in its own
  `BEGIN IMMEDIATE`. Reopens with a different `embed_dim` raise
  `MemoryError::DimMismatch`. Migrations are idempotent.
- `store.rs:60` `SqliteStore` — the only SQL writer. `open` registers
  sqlite-vec via process-wide `sqlite3_auto_extension`, then opens the
  per-process connection with WAL + foreign_keys + synchronous=NORMAL.
  `insert` writes `memories` and `memory_vec` in one transaction so
  the kill-mid-tx invariant holds; `delete` is symmetric. `knn` takes
  an optional `scope_key: Option<&str>` filter alongside the existing
  `session_id` / `tier_floor` filters.
- `lexical.rs:48` `LexicalIndex` — tantivy 0.22 BM25 index over the
  schema `(id, session_id, scope_key, tier, content)`. `upsert` does
  delete-by-term-then-add; `commit()` is explicit and reloads the
  reader. `search` takes an optional `scope_key: Option<&str>` filter.
- `search.rs:6` `HybridSearchConfig` — knobs from `CLUD_MEMORY_RRF_K`
  (default 60) and `CLUD_MEMORY_MAX_RESULTS` (default 50).
  `search.rs:51` `rrf_fuse` — reciprocal-rank-fusion of BM25 + vec
  hits, sorted desc by score with stable insertion-order ties. The
  scope filter is applied upstream on both `knn` and `search`, so
  fusion stays unaware of scoping (pure rank math).
- `tiers.rs:55` `TierConfig` — retention knobs (working TTL, promote
  access floor, promote dwell, decay half-life). `tiers.rs:84`
  `from_env` overrides defaults from `CLUD_MEMORY_WORKING_TTL_MS`,
  `CLUD_MEMORY_PROMOTE_ACCESS_FLOOR`, `CLUD_MEMORY_PROMOTE_DWELL_MS`,
  `CLUD_MEMORY_DECAY_HALF_LIFE_MS`.
- `tiers.rs:117` `promote_candidates` — pure read returning rows whose
  `access_count >= promote_access_floor` and dwell since
  `tier_change_at_ms` clears `promote_dwell_ms`. Walks Working →
  Episodic and Episodic → Semantic.
- `tiers.rs:150` `apply_promotions` — applies the promotion list,
  calling `store.promote_tier` then `lexical.upsert` per row so BM25
  tier stays in lockstep with SQLite. Commits the lexical writer.
- `tiers.rs:183` `retention_score` — `[0, 1]` blend of recency decay
  (half-life-based), an access-count boost, and a tier floor (Working
  0.0, Episodic 0.25, Semantic 0.5). Pure function; not used by
  auto-forget — surface-ranking only.
- `tiers.rs:207` `forget_expired` — deletes Working rows whose
  `now_ms - last_access_at_ms > working_ttl_ms` from both the SQLite
  store and the lexical index. Episodic and Semantic are never
  auto-deleted ([DD-016](../../../../docs/DESIGN_DECISIONS.md#dd-016-three-tier-auto-forget-is-scoped-to-working-only)).
- `tiers.rs:234` `tier_exportable` — git-artifact serialization hook
  for sibling #264. Working = never, Semantic = always, Episodic =
  policy-configurable on `TierConfig`.

The consolidation timer / `tick()` driver, Stop-hook callers, daemon
spawn glue, and the MCP `memory_consolidate` tool live in sibling
sub-issues; this directory only exposes the primitives above.

## Identity & scoping

`identity.rs` answers: which agent-memory bucket should this working
tree read and write? The primary key is the **normalized `origin`
URL**, computed by `normalize_origin_url`. Branch is **metadata, not a
partition** ([DD-014](../../../../docs/DESIGN_DECISIONS.md#dd-014-repo-url-as-primary-memory-scope-branch-as-metadata-not-partition)),
so cross-branch memory continuity is the default.

- `RepoScope` (identity.rs:24) — `{ key, origin_url, common_dir, branch,
  is_orphan, is_worktree, branch_isolated }`. `key` is composed by
  `scope_key`; everything else is provenance so callers can render
  *why* a key was chosen without re-running git.
- `resolve_repo_scope(cwd)` (identity.rs:52) — runs `git rev-parse
  --git-common-dir` (worktree-aware), then `git remote get-url origin`,
  then `git symbolic-ref --short HEAD`, then orphan detection against
  `origin/HEAD`, then the branch-isolate marker.
- `normalize_origin_url` (identity.rs:90) — strips `.git`, trims
  trailing slashes, lowercases scheme + host (path stays case-sensitive),
  drops the default port for the detected scheme (`:22` for ssh,
  `:443` for https, `:80` for http, `:9418` for git://). SCP-style ssh
  remotes (`git@host:path`) are rewritten to `ssh://git@host/path`.
- `scope_key` (identity.rs:127) — `repo://<normalized-origin>` when
  origin is present, `dir://<canonical-common-dir>` fallback, with
  `#branch=<name>` appended when the working tree opted into
  isolation.
- `branch_isolate(common_dir)` / `branch_unisolate(common_dir)`
  (identity.rs:143 / identity.rs:155) — write/remove
  `<common_dir>/.clud/memory-branch-isolate`. The marker file is the
  opt-out: when present, the current branch is treated as its own
  scope partition. The CLI verb `clud memory branch-isolate` is owned
  by #262 (CLI surface); this module exposes the library functions.
- `cross_repo_glob_filter(globs)` (identity.rs:166) — pure shell-glob
  predicate for `--repo all` / `--repo-glob` style cross-repo
  searches (consumer wiring lives in #259 / #262).

Worktrees share scope automatically — `git rev-parse --git-common-dir`
returns the primary's common dir from any linked worktree, so all
worktrees of one clone resolve to the same `scope_key`. Orphan
branches inherit their parent's scope by default; the user opts out
per-branch via the marker file.

## Embedded schemas

`crates/clud-bin/schema/memory_v1.sql` is `include_str!`'d by
`schema.rs`. `{embed_dim}` is the only template variable; everything
else is literal SQL. A small sidecar `memory_meta(key, value)` table
pins the dim chosen at first open so subsequent opens can detect a
mismatch without parsing the `vec0` virtual-table DDL.

`crates/clud-bin/schema/memory_v2.sql` is the forward-only ALTER-TABLE
delta for #267: adds `memories.scope_key`, `memories.branch_name`,
`memories.is_orphan`, and `idx_memories_scope`. Existing rows on a v1
database keep `scope_key = NULL` (= global, matches `session_id = NULL`
semantics).

## Cross-process

Out of scope for this directory; daemon-integration sub-issue wires
`Mutex<SqliteStore>` lifetime inside `__daemon` and exposes
`checkpoint_truncate` to the GC tick.
