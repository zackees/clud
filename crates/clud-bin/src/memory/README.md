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
- `git_artifact.rs` — issue #264 git-artifact writer/reader. Owns
  `export_to_disk` / `import_from_disk` and the `PrivacyFilter` that
  parses `<root>/.cludignore`. See "Git-artifact serialization" below.

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

## MCP server

The agent-memory subsystem is surfaced to Claude Code / Codex via an
in-daemon MCP server (issue #259). Implementation lives in
[`crates/clud-bin/src/daemon/memory_mcp.rs`](../daemon/memory_mcp.rs);
the stdio bridge lives in
[`crates/clud-bin/src/mcp_bridge.rs`](../mcp_bridge.rs).

The 8 ESSENTIAL_TOOLS exposed (verbatim from agentmemory):

- `memory_save` — embed + persist a memory row.
- `memory_recall` — fetch one row by id.
- `memory_smart_search` — hybrid RRF (BM25 + vector) search.
- `memory_sessions` — distinct `session_id` values.
- `memory_consolidate` — drive one consolidation tick on demand.
- `memory_diagnose` — basic health (embedder, dim, row count, schema
  user_version). Subsystem-level checks (actions/leases/sentinels/…)
  land in a future PR.
- `memory_lesson_save` — insert into the `lessons` table.
- `memory_reflect` — **documented stub** until v0.5; returns an
  `unimplemented` JSON-RPC error today. Knowledge-graph + LLM reflection
  lands with the v0.5 release per META #255.

Scope filter wiring (`scope_key` end-to-end) lands in #267 and is
already on main; this MCP surface accepts the `scope_key` argument on
`memory_smart_search` and forwards it to `SqliteStore::knn` and
`LexicalIndex::search`.

### Manual MCP registration (v0.1)

Auto-registration of `~/.claude.json` / `~/.codex/config.toml` is owned
by sibling #265. For v0.1 the user adds an entry by hand:

```jsonc
// ~/.claude.json
{
  "mcpServers": {
    "clud-memory": {
      "command": "clud",
      "args": ["mcp"]
    }
  }
}
```

```toml
# ~/.codex/config.toml
[mcp_servers.clud-memory]
command = "clud"
args = ["mcp"]
```

`clud mcp` calls `daemon::ensure_daemon` so a cold first launch
transparently brings the clud daemon up; subsequent connects are
loopback TCP only.

## CLI surface (#262)

`cli.rs` dispatches `clud memory <verb>` to thin handlers that proxy
through the daemon's `/memory/*` HTTP routes. The daemon owns the
single SQLite writer per process; the CLI's job is to embed the user's
text where needed, hit HTTP, and pretty-print the JSON response. See
[DD-018](../../../../docs/DESIGN_DECISIONS.md#dd-018-clud-memory-cli-verbs-proxy-mutating-ops-through-the-daemon).

| Verb | Action | Daemon route |
|---|---|---|
| `init` | Best-effort embedder warm; prints resolved paths + embed_dim | `GET /memory/stats` |
| `status [--json]` | Tier counts, embedder name + dim, schema user_version | `GET /memory/stats` |
| `search <q> [-k N] [--session-id] [--tier-floor] [--scope-key] [--json]` | RRF hybrid BM25 + KNN search | `GET /memory/search?q=&k=&...` |
| `save <content> [--tier] [--session-id] [--metadata] [--json]` | Embed + insert | `POST /memory/save` |
| `forget <id> [--json]` | Delete row, cascade to vec + tantivy | `POST /memory/forget/<id>` |
| `export [--to-stdout]` | Dump recent rows as JSON-lines | `GET /memory/recent?limit=100000` |
| `export --to-disk [--include-episodic] [--allow-private]` | Write `.clud/memory/*.md` (#264) | none — local fs |
| `import --from-stdin` | Read JSON-lines from stdin, re-save each | `POST /memory/save` per line |
| `import --from-disk [--include-episodic]` | Re-insert rows from `.clud/memory/` (#264) | none — local fs |
| `ui [--no-open]` | Open dashboard at `#memory` | reads daemon-info |
| `reembed [--model] [--dry-run]` | Count rows (dry-run) or note that live reembed needs the daemon stopped | `GET /memory/stats` |
| `branch-isolate` | Write `<common-dir>/.clud/memory-branch-isolate` | none — local fs |
| `branch-unisolate` | Remove the marker | none — local fs |

Exit codes: `0` success, `1` user error (validation / missing query /
empty content), `2` internal error (HTTP 5xx / JSON decode failure /
non-git repo for branch-isolate), `3` daemon unavailable (including
`--no-daemon`).

## Dashboard (#263)

The daemon's bundled SPA at
[`crates/clud-bin/assets/dashboard/index.html`](../../assets/dashboard/index.html)
includes a fifth "Memory" tab. Anchor `#memory` selects it on load so
`clud memory ui` can deep-link straight to the tab. The tab is plain
vanilla JS — no React, no build step — matching the existing four tabs
([DD-020](../../../../docs/DESIGN_DECISIONS.md#dd-020-memory-dashboard-tab-stays-in-the-vanilla-js-spa-pattern)).

Route dependencies (all loopback HTTP, served by `daemon::http`):

- `GET /memory/stats` — five-tile stats card (total rows, tier
  counts, embedder status, schema user_version). Polled every 5s
  whenever the Memory tab is active; the underlying tab refresh
  hook (`refreshMemoryTab`) is also a no-op when another tab is
  visible so the SQLite mutex stays cold for users on other tabs.
- `GET /memory/recent?limit=50` — recent-memory table (tier badge,
  truncated content + full-content tooltip, short session id, age,
  forget button). Polled in lockstep with `/memory/stats`.
- `GET /memory/search?q=&k=` — search input results.
  Submit-on-enter (no debounce — the dashboard's search is explicit
  by design so the user can use long natural-language queries
  without partial-query rerank churn).
- `POST /memory/save` — collapsed `<details>` card; expands on click.
- `POST /memory/forget/<id>` — per-row `×` button with `confirm()`.

ASCII wireframe of the tab:

```
+---------------------------------------------------------+
| Memory (123)                                            |
| +-----+-----+-----+-----+-----+-----+                   |
| |Total|Work |Epis |Sem  |Emb  |Schm |  <- stats card    |
| | 123 | 100 | 20  |  3  |ready| v2  |                   |
| +-----+-----+-----+-----+-----+-----+                   |
+---------------------------------------------------------+
| Search [_______________] [25 v] [Search]                |
|   no hits yet                                           |
+---------------------------------------------------------+
| + Save a new memory  (expands to textarea + tier + Save)|
+---------------------------------------------------------+
| Recent memories (auto-refresh 5s)                       |
|   tier | content                | session | age |   x   |
|   work | "fix the daemon..."    | abcdefg | 3m  |   x   |
|   epis | "lesson: always..."    | hijklmn | 1h  |   x   |
+---------------------------------------------------------+
```

## Git-artifact serialization (#264)

`git_artifact.rs` writes the durable tiers of the store as a tree of
YAML-frontmatter Markdown files under `<git-root>/.clud/memory/` so
agent memory can be committed alongside the code it describes.

```
<git-root>/.clud/memory/
  .cludignore                    # privacy filter (committed)
  semantic/<ULID>-<slug>.md      # always exported
  episodic/<ULID>-<slug>.md      # opt-in via --include-episodic
  relations.jsonl                # append-only edge log
```

Each file is `<ULID>-<slug>.md`. The ULID prefix is fresh per export
(filesystem-sortable listings); the canonical id stays in the
frontmatter as a uuidv7 `id:` field. Slug = up to 40 chars of the
first non-blank line, lowercased and collapsed to `[a-z0-9-]`. Empty
slug falls back to `memory`.

### Frontmatter shape

```yaml
---
id: 01931d2b-7c0e-7d2c-...        # original MemoryId (uuidv7)
tier: semantic
session_id: null
scope_key: repo://github.com/zackees/clud
branch_name: null
is_orphan: false
created_at_ms: 1700000000000
updated_at_ms: 1700000000000
tier_change_at_ms: 1700000000000
access_count: 3
last_access_at_ms: 1700000000000
metadata: {}
private: false                    # if true, skipped at export
---

<markdown body == row.content>
```

### `.cludignore`

Lines starting with `#` are comments. Lines starting with `body-regex:`
compile to a regex matched against the row's body; every other line is
a shell-style glob matched against the row's `scope_key` or
`session_id`. A row is skipped at export when **any** rule matches, or
when `metadata.private == true` (the latter always wins; pass
`--allow-private` to override).

```
# .cludignore
body-regex: AKIA[0-9A-Z]{16}
body-regex: (?i)password\s*=
session-xyz-*
*github.com/secret-repo*
```

### Tier-gated visibility

- Working — never exported (DD-016 reflexively: working is transient).
- Episodic — opt-in via `CLUD_MEMORY_EXPORT_EPISODIC=1` or
  `clud memory export --to-disk --include-episodic`.
- Semantic — always exported.

### Relations log

`relations.jsonl` is append-only and idempotent. `export_to_disk`
walks `memory_relations` and appends only rows whose
`(src_id, dst_id, kind)` are not already in the file.
`import_from_disk` replays the file with `INSERT OR IGNORE`.

### CLI seams

| Verb | Effect |
|---|---|
| `clud memory export --to-disk [--include-episodic] [--allow-private]` | Walk the daemon-owned store and write `.clud/memory/*.md` under the current git repo. |
| `clud memory import --from-disk [--include-episodic]` | Walk `.clud/memory/*.md` and re-insert rows not already in the store. Re-embeds via the configured embedder. |

Both verbs talk to the on-disk SQLite file directly (not via the
daemon's HTTP routes) because the export is a read-only walk and WAL
permits concurrent readers. The import path opens the store with the
embedder's `dim()` and is best run with the daemon stopped to avoid
double-loading the embedder.
