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

## Identity and scoping (#267)

Every saved memory carries a **`scope_key`** that partitions one
working tree's memories from another's. The key is built by
[`memory::identity::scope_key`](../../crates/clud-bin/src/memory/identity.rs):

| Working tree state | `scope_key` |
|---|---|
| `git remote get-url origin` returns a value | `repo://<normalize_origin_url(url)>` |
| No origin remote | `dir://<canonical-common-dir>` |
| Branch-isolate marker present at `<common_dir>/.clud/memory-branch-isolate` | base key + `#branch=<branch>` |

`normalize_origin_url` collapses equivalent SSH/HTTPS pairs so
`git@github.com:Foo/Bar.git` and `https://github.com/Foo/Bar/` map to
the same key. Rules: strip trailing `.git`, trim trailing slashes,
lowercase scheme + host (path stays case-sensitive), drop the default
port for the detected scheme (`:22` for ssh, `:443` for https, `:80`
for http, `:9418` for git://). SCP-style ssh inputs become
`ssh://user@host/path`.

### Worktree policy

`git rev-parse --git-common-dir` is the canonical path used by
`scope_key`. Linked worktrees of one clone all return the primary's
common dir, so every worktree of one clone resolves to the same
`scope_key` and shares its memory bucket. No special-case wiring;
the worktree-vs-primary distinction is exposed only as
`RepoScope::is_worktree` provenance.

### Orphan branch policy

Orphan branches (`git merge-base HEAD origin/HEAD` returns non-zero)
**inherit** their parent clone's scope by default — the rationale is
that "auth uses HS256 JWTs from vault" is a project fact, not a branch
fact ([DD-014](../DESIGN_DECISIONS.md#dd-014-repo-url-as-primary-memory-scope-branch-as-metadata-not-partition)).
`RepoScope::is_orphan` is recorded as provenance.

### Branch isolation opt-out

A user who *wants* the current branch to keep its memories private
writes a marker file:

```
<common_dir>/.clud/memory-branch-isolate
```

When present, `scope_key` returns base + `#branch=<branch>` instead
of just base. The marker travels with the working tree (committed if
the user wants the decision to persist across clones). The CLI verb
`clud memory branch-isolate` is owned by #262 — this directory only
exposes the library functions `branch_isolate(common_dir)` and
`branch_unisolate(common_dir)`.

### Schema impact

The v1 → v2 migration adds three columns to `memories`:

- `scope_key TEXT` (nullable) — repo-level partition key from
  `scope_key`. `NULL` means "global" (matches `session_id = NULL`
  semantics: null filters do not partition).
- `branch_name TEXT` (nullable) — current branch at save time.
  Provenance only.
- `is_orphan INTEGER NOT NULL DEFAULT 0` — orphan-branch flag at save
  time. Provenance only.

Plus `idx_memories_scope ON memories(scope_key)`.

`SqliteStore::knn` and `LexicalIndex::search` both accept an optional
`scope_key: Option<&str>` filter alongside the existing session/tier
filters. `rrf_fuse` stays scope-unaware (pure rank math) — the filter
is applied upstream on both ranking sources.

## Sibling sub-issues (META #255)

- Embeddings (local + remote + Windows-ARM strategy)
- Tier lifecycle (Working / Episodic / Semantic + decay + promotion)
- MCP server in daemon + `clud mcp` stdio bridge
- Daemon HTTP/IPC routes for memory ops
- `clud memory search` / `save` CLI verbs (including
  `clud memory branch-isolate`)
- Cross-process persistence test
- Knowledge graph (deferred past v1)

Each sub-issue lands on top of the public surface this PR exposes from
[`memory/mod.rs`](../../crates/clud-bin/src/memory/mod.rs); none of
them need to touch the storage layer itself.
