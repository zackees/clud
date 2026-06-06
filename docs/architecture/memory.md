# Agent memory

Cross-cutting sketch of the agent-memory subsystem. Source of truth for
the storage layer; sibling sub-issues under META #255 will fill in the
embedder, MCP server, daemon-IPC routes, retention/decay policy,
knowledge-graph, and CLI verbs.

## Status

PR1 shipped the storage + hybrid-search foundation. PR2 (issue #258,
this commit) adds the tier-lifecycle primitives:

- `crates/clud-bin/src/memory/` — SqliteStore, LexicalIndex, RRF fusion,
  embedded `memory_v1.sql` migration runner, tier lifecycle.
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

## Embedder

PR2 (#257) adds the embedder abstraction under
[`crates/clud-bin/src/memory/embedder/`](../../crates/clud-bin/src/memory/embedder/README.md).
Three kinds:

- `Local(LocalEmbedder)` — `fastembed::TextEmbedding` with
  MiniLM-L6-v2 (384-dim). Cached under `<state_dir>/memory/models/`.
  Gated on the `memory_local_embed` Cargo feature and on
  `cfg(not(all(target_arch = "aarch64", target_os = "windows")))` —
  no ort prebuilt for Windows-ARM today; same carve-out shape
  `whisper-rs` uses ([DD-015](../DESIGN_DECISIONS.md#dd-015-local-embedder-via-fastembed--windows-arm-carve-out)).
- `Remote(RemoteEmbedder)` — pure-`ureq` HTTP client. Four providers:
  Anthropic (Voyage), OpenAI, Gemini, Ollama. Selected via
  `CLUD_MEMORY_EMBEDDER_PROVIDER`.
- `Disabled { reason }` — explicit no-op. `embed()` returns
  `MemoryError::EmbedderDisabled` with the four-path remediation
  message (remote API key / Ollama on LAN / WSL2 / wait for ort 2.0).

`embedder_from_env()` picks one based on the documented env var
ladder. Storage layer does not own the embedder — `SqliteStore` still
takes `&[f32]` slices. The library primitive `reembed_all(store,
embedder)` walks every row and rewrites the vec table; the
`clud memory reembed` CLI verb lands in #262 and wraps this with
`--resume` checkpointing.

### Recipe: Ollama on a sibling x86/Linux/macOS box (Windows-ARM workaround)

On the sibling (Linux / macOS / Windows-x64):

```
ollama pull nomic-embed-text
OLLAMA_HOST=0.0.0.0:11434 ollama serve
```

Verify reachable:

```
curl http://<host>:11434/api/tags
```

On the Windows-ARM clud client:

```
setx CLUD_MEMORY_EMBEDDER_PROVIDER ollama
setx CLUD_MEMORY_EMBEDDER_URL      http://<host>:11434/api/embeddings
```

Restart the daemon. Switching the embedder model after rows exist
will need `clud memory reembed --model nomic-embed-text` (#262) to
rewrite the vec table at the new 768-dim.

## Tier lifecycle (issue #258)

Three tiers with one-way promotion and tier-gated auto-forget:

- **Working** — per-session scratch. Auto-forgotten when
  `now_ms - last_access_at_ms > working_ttl_ms` (default 24 h).
- **Episodic** — session-summarized; manually deleted only.
- **Semantic** — durable cross-session knowledge; manually deleted only.

Auto-forget is **scoped to Working only**
([DD-016](../DESIGN_DECISIONS.md#dd-016-three-tier-auto-forget-is-scoped-to-working-only)).
Episodic and Semantic survive until explicit deletion; the retention
score still ranks them for surface UIs but does not feed the delete
path.

Promotion is one-way (Working → Episodic → Semantic). A row is a
promotion candidate when both gates clear:

- `access_count >= TierConfig.promote_access_floor` (default 3).
- `now_ms - tier_change_at_ms >= TierConfig.promote_dwell_ms` (default
  1 h). The dwell gate prevents thrash near the access-count
  threshold.

[`memory::tiers`](../../crates/clud-bin/src/memory/tiers.rs) exposes
four primitives, designed for a future consolidation timer (sibling
sub-issue #261) to call on a periodic tick:

1. `promote_candidates(store, now_ms, cfg) -> Vec<(MemoryId, Tier)>`
   — pure read.
2. `apply_promotions(store, lexical, &promotions, now_ms)` — applies
   the list and keeps the BM25 tier field in lockstep with SQLite.
3. `retention_score(row, now_ms, cfg) -> f32` in `[0, 1]` blending
   recency decay (half-life), access boost, and a tier floor (Working
   0.0, Episodic 0.25, Semantic 0.5).
4. `forget_expired(store, lexical, now_ms, cfg) -> usize` — deletes
   expired Working rows, returns the count.

`tier_exportable(tier, cfg) -> bool` is the seam for the git-artifact
serialization sub-issue (#264): Working = never, Semantic = always,
Episodic = policy-configurable on `TierConfig`.

## Git-artifact serialization (#264)

`memory::git_artifact` writes the durable tiers of the store as a tree
of YAML-frontmatter Markdown files under `<git-root>/.clud/memory/` so
agent memory can be checked into the repo and travel with it across
machines and team members.

```
<git-root>/.clud/memory/
  .cludignore                    # privacy filter (committed)
  semantic/<ULID>-<slug>.md
  episodic/<ULID>-<slug>.md      # opt-in
  relations.jsonl                # append-only edge log
```

The filename ULID is fresh per export (filesystem-sortable); the
canonical id lives in the YAML frontmatter as the original uuidv7
`id:` field. Frontmatter carries every column from `memories` plus a
`private:` flag so collaborators can mark a row as off-limits to
export without editing `.cludignore`.

### `.cludignore`

Conservative default privacy filter at `<root>/.cludignore`:

- `body-regex: <regex>` lines compile to regexes matched against the
  row's body. Used for AKIA / sk- / Bearer / private-key / etc.
  patterns. Invalid regex returns an error rather than silently
  ignoring the line.
- Other non-comment lines are shell-style globs matched against the
  row's `scope_key` or `session_id`. `*`, `?`, literals only — no
  path semantics because scope keys are not paths.
- A row's `metadata_json` may contain `"private": true`; this always
  wins regardless of `.cludignore`. Pass `--allow-private` to override.

A row is skipped at export when **any** rule matches. The same
`.cludignore` is read on import, but the import path does not
re-filter — files on disk are authoritative.

### Tier policy

Default: Semantic only. Episodic widens with
`CLUD_MEMORY_EXPORT_EPISODIC=1` or `--include-episodic` on the CLI.
Working is never exported (DD-016). This matches `tier_exportable`.

### Relations log

`relations.jsonl` is append-only and idempotent. `export_to_disk`
walks the `memory_relations` SQL table and appends rows whose
`(src_id, dst_id, kind)` are not yet on disk. `import_from_disk`
replays the file via `INSERT OR IGNORE`. Hand-edits are tolerated.

### Atomicity

Each `.md` write is `tempfile::NamedTempFile` + `persist` (rename) so
a crash mid-write leaves either the previous version or no file — never
a half-written body. Concurrent exporters racing on the same file
collide deterministically at the rename step.

### CLI seams

- `clud memory export --to-disk [--include-episodic] [--allow-private]`
- `clud memory import --from-disk [--include-episodic]`

Both verbs talk to the on-disk SQLite file directly (read-only walks
the WAL safely allows) rather than the daemon's HTTP routes. The
import path needs the embedder to produce vectors for newly-inserted
rows; for large imports the user should stop the daemon first so the
embedder is loaded only once.

### Auto-export on Stop hook

The Stop-hook wiring lives in sub-issue #260. When the hook fires, it
calls `git_artifact::export_to_disk` with the default policy unless
`CLUD_MEMORY_NO_AUTO_EXPORT=1` is set. As of the PR shipping #264 the
hook itself is not yet on main; the seam is exposed and ready.

Env-var knobs (read by `TierConfig::from_env`):

- `CLUD_MEMORY_WORKING_TTL_MS` (default 86_400_000)
- `CLUD_MEMORY_PROMOTE_ACCESS_FLOOR` (default 3)
- `CLUD_MEMORY_PROMOTE_DWELL_MS` (default 3_600_000)
- `CLUD_MEMORY_DECAY_HALF_LIFE_MS` (default 604_800_000)

## Daemon integration (issue #261)

The memory subsystem runs **in-process** inside the existing `clud`
daemon, not as a sidecar. One daemon owns the whole subsystem; the GC
service and dashboard listener already established that pattern
([DD-017](../DESIGN_DECISIONS.md#dd-017-memory-service-runs-in-process-inside-the-existing-clud-daemon)).

`daemon::memory_service::spawn_memory_service(state_dir)` opens every
resource the rest of the daemon needs:

1. Resolve `<state_dir>/memory/{memory.db, tantivy/}` and create the
   directories if missing.
2. Resolve the embedder via `embedder_from_env()`. The embedder's `dim`
   drives the SQLite vec0 column width on first open; on subsequent
   opens a mismatch is logged as a warning and the daemon keeps going
   (`clud memory reembed` — #262 — is the manual fix).
3. Open `SqliteStore` (runs schema migrations to user_version 2) and
   run `PRAGMA wal_checkpoint(TRUNCATE)` for WAL recovery on every
   start.
4. Open `LexicalIndex` (tantivy half-segments from a crash are dropped
   on open).
5. **Reconciliation pass**: walk every SQLite row by tier and re-upsert
   it into tantivy. Bounded by row count; cheap on a clean daemon. The
   pass self-heals the eventual-consistency window between SQLite
   commit and tantivy commit so a daemon kill mid-write doesn't leave
   BM25 permanently missing the row.
6. Wrap `SqliteStore` and `LexicalIndex` in `Arc<Mutex<...>>`; wrap
   `Embedder` in `Arc<_>` (no internal mutex — `Embedder` is
   `Send + Sync`). Resolve `TierConfig::from_env`. Return
   `MemoryService { store, lexical, embedder, tier_config,
   consolidate_interval_ms }`.

### Consolidation timer

A named thread `clud-memory-consolidate` ticks every
`CLUD_MEMORY_CONSOLIDATE_INTERVAL_MS` (default 300_000 = 5 min). Per
tick:

1. `promote_candidates(&store, now_ms, &cfg)`.
2. `apply_promotions(&mut store, &mut lexical, &promotions, now_ms)`.
3. `forget_expired(&mut store, &mut lexical, now_ms, &cfg)`.
4. Every `CLUD_MEMORY_CHECKPOINT_EVERY_N_TICKS` ticks (default 12 = 1
   hour at the default cadence), `store.checkpoint_truncate()` to
   truncate the WAL.
5. Log a structured one-liner with promoted / forgotten counts and the
   checkpoint flag.

The orchestration loop polls the shared shutdown flag at one-tenth the
interval so cooperative shutdown wakes within seconds. The
`run_one_consolidation_tick` helper is `pub(crate)` so tests can drive
the lifecycle math with a deterministic `now_ms`.

### Concurrency

| Resource | Lock kind | Writers | Readers |
|---|---|---|---|
| `SqliteStore` | `Arc<Mutex<...>>` | one at a time (the timer thread + HTTP save handler) | through the same mutex; WAL still allows multi-reader inside the same process |
| `LexicalIndex` | `Arc<Mutex<...>>` | one `IndexWriter` per process | reader is materialized inside the lock |
| `Embedder` | `Arc<...>` | none — no internal mutex | shared across the timer + every HTTP handler |

Order matters on a save: take SQLite first, commit, drop, then take
tantivy. The reconciliation pass on next boot covers the eventual-
consistency window if a crash happens between the two commits.

### HTTP routes (live as of #262)

The dashboard's existing `tiny_http` server (issue #183) carries five
new routes wired to the live `Arc<MemoryService>`:

- `GET /memory/recent?limit=N` — returns newest-first rows as
  `[{id, tier, content, session_id, scope_key, created_at_ms, access_count}]`.
- `GET /memory/search?q=…&k=…&session_id=…&tier_floor=…&scope_key=…`
  — embeds the query (if the embedder is live), runs BM25 + KNN, RRF-
  fuses, and returns `[{id, tier, content, score, ...}]`. When the
  embedder is `Disabled`, the route degrades to BM25-only.
- `GET /memory/stats` — returns `{tier_counts, embedder_status,
  embedder_dim, store_embed_dim, schema_user_version,
  consolidate_interval_ms}`.
- `POST /memory/save` — body `{content, tier?, session_id?, scope_key?,
  metadata_json?}`; embeds + inserts + commits the lexical writer in
  the documented SQLite-first order; returns `{id, tier, created_at_ms}`.
- `POST /memory/forget/<id>` — deletes the row from both `memories` +
  `memory_vec` (one transaction) and the tantivy index; returns
  `{id, forgotten: bool}`.

Validation: missing/empty `q`, empty `content`, unknown tier name, or
ill-formed id return 400 with a JSON error body. When `MemoryService`
is `None` (subsystem failed to start) every route returns 503 with a
JSON error body. The daemon keeps serving sessions and GC traffic.

### CLI surface (#262)

`crates/clud-bin/src/memory/cli.rs::run` dispatches `clud memory <verb>`.
See [`crates/clud-bin/src/memory/README.md`](../../crates/clud-bin/src/memory/README.md#cli-surface-262)
for the verb-to-route table. The CLI proxies **all mutating ops through
the daemon** so there is exactly one SQLite writer per process
([DD-018](../DESIGN_DECISIONS.md#dd-018-clud-memory-cli-verbs-proxy-mutating-ops-through-the-daemon)).
`branch-isolate` and `branch-unisolate` are the two verbs that
**don't** touch the daemon — they write/remove a marker file under
`<git common-dir>/.clud/memory-branch-isolate`. The `--to-disk` /
`--from-disk` flags on `export` and `import` are stubs that point
users at #264.

Exit codes: `0` success, `1` user error, `2` internal error
(HTTP 5xx / decode), `3` daemon unavailable.

The MCP server itself (`#259`) and the hook subcommands (`#260`) plug
into `MemoryService` from outside; this PR just shares the four handles
the way #135 shared the GC mpsc channel.

`DaemonInfo.memory_mcp_port` is now populated by #259 — see "MCP server"
below.

## Hook subcommands (#260)

`clud hook <verb>` exposes four hidden subcommands that Claude Code /
Codex invoke at session lifecycle events. Handlers live in
[`crates/clud-bin/src/hooks.rs`](../../crates/clud-bin/src/hooks.rs);
file-by-file detail and payload shapes are in
[`crates/clud-bin/src/memory/README.md`](../../crates/clud-bin/src/memory/README.md#hooks-260).

Why a CLI verb and not direct daemon shell-out? Same rationale as
[DD-019](../DESIGN_DECISIONS.md#dd-019-clud-memory-cli-verbs-proxy-mutating-ops-through-the-daemon)
(and the new DD-020): hooks are short-lived subprocesses; the daemon
is the single SQLite writer. HTTP keeps the wire seam aligned with the
`clud memory` CLI and the dashboard.

### Lifecycle

1. **`session-start`** — Claude/Codex invokes once when a session
   opens. The handler reads the payload, calls `/memory/recent`
   client-side filtered by `session_id`, and writes a
   `<context source="clud-memory">` block to stdout. Claude injects the
   block into the system prompt; Codex surfaces it as a visible system
   message.
2. **`user-prompt-submit`** — invoked per user prompt. The handler
   looks for an opt-in directive (`remember:`, `save this:`, …) and
   POSTs `/memory/save` only when one is present. Otherwise no-op. The
   defaults are conservative so the hook does not silently log every
   prompt.
3. **`post-tool-use`** — invoked after every tool call. v0.1 is a
   logged no-op; tool-output classification lands in v0.5 alongside a
   `/memory/working/append` route.
4. **`stop`** — invoked at session end (Claude `Stop`; Codex
   `session_end`). When `CLUD_MEMORY_AUTO_CONSOLIDATE_ON_STOP=1` the
   handler will POST to `/memory/consolidate` once that route exists
   (TODO; the consolidation timer in the daemon owns the schedule
   today). Default off.

### Failure model

Every hook **exits 0 unconditionally**. A failing hook must never
block the agent. Stdin payload errors, daemon-unreachable errors, and
HTTP 5xx all silently exit 0; the only way to see diagnostics is to
set `CLUD_MEMORY_DEBUG_HOOKS=1`, which routes one structured line per
hook to stderr (parse failure, daemon unreachable, save failure).
Auto-export of recalled rows to disk and the `post-tool-use`
classifier are owned by siblings #264 and v0.5 respectively.

### Registration

Out of scope for #260. Sibling #265 owns writing the per-frontend
hook config (`~/.claude/settings.json` for Claude Code,
`~/.codex/hooks.json` for Codex). The four `clud hook` subcommands are
hidden from `--help` (registered with `clap`'s `hide = true`) because
they are not user-facing CLI verbs.

## MCP server

Issue #259 surfaces the agent-memory subsystem as an MCP server,
in-process with the rest of the daemon (see
[DD-018](../DESIGN_DECISIONS.md#dd-018-mcp-server-embedded-in-clud-daemon-vs-sidecar-binary)
for why we embed rather than ship a sidecar). The implementation is in
[`crates/clud-bin/src/daemon/memory_mcp.rs`](../../crates/clud-bin/src/daemon/memory_mcp.rs);
the `clud mcp` stdio↔TCP bridge is in
[`crates/clud-bin/src/mcp_bridge.rs`](../../crates/clud-bin/src/mcp_bridge.rs).

### Port allocation

`spawn_mcp_server` binds `127.0.0.1:0` (ephemeral loopback) and writes
the resolved port into `DaemonInfo.memory_mcp_port`. The accept loop is
a single `std::thread`; each accepted TCP connection spawns its own
per-connection thread. There is no tokio runtime in the daemon — the
existing `Arc<Mutex<...>>` resources on `MemoryService` are reused
unchanged.

### The 8 tools (agentmemory `ESSENTIAL_TOOLS`)

Names and argument schemas are 1:1 with the `rohitg00/agentmemory`
TypeScript reference:

| Tool | v0.1 status |
|---|---|
| `memory_save` | functional |
| `memory_recall` | functional |
| `memory_smart_search` | functional (BM25 + vec → RRF) |
| `memory_sessions` | functional |
| `memory_consolidate` | functional (manual tick) |
| `memory_diagnose` | basics: embedder, dim, row count, schema user_version |
| `memory_lesson_save` | functional (writes `lessons` table) |
| `memory_reflect` | **documented stub** — returns an internal error until v0.5 (depends on knowledge graph + LLM provider) |

### Wire protocol

Line-delimited JSON-RPC 2.0 over TCP. Supported methods: `initialize`,
`tools/list`, `tools/call`. Tool results return the
`{ content: [{type: "text", text: "<json-string>"}] }` shape mandated
by the MCP spec. Errors use the standard JSON-RPC codes plus `-32099`
("daemon unavailable") emitted by the bridge when
`DaemonInfo.memory_mcp_port` is `None`.

### `clud mcp` stdio bridge

Calls `daemon::ensure_daemon` first (transparently brings the daemon up
if it isn't running), reads the port from `daemon.json`, then opens a
loopback TCP socket and proxies bytes between stdio and the socket
using two `std::thread`s. Closes when either side closes. On error
(daemon unavailable, missing port, bad connect) the bridge emits a
single JSON-RPC error envelope on stdout — never hangs.

### Manual registration (v0.1)

Auto-registration of `~/.claude.json` / `~/.codex/config.toml` is owned
by sibling #265 and is not in #259. For v0.1 the user adds an entry by
hand — see
[`crates/clud-bin/src/memory/README.md`](../../crates/clud-bin/src/memory/README.md#manual-mcp-registration-v01).

## Dashboard (#263)

The daemon's bundled SPA — `crates/clud-bin/assets/dashboard/index.html`,
served at `GET /` by `daemon::http` — gains a fifth "Memory" tab. Pure
vanilla JS by design, see
[DD-020](../DESIGN_DECISIONS.md#dd-020-memory-dashboard-tab-stays-in-the-vanilla-js-spa-pattern)
for why no React / no build step.

The tab consumes the four read routes already shipped by #261/#262:

- `GET /memory/stats` drives the stats card (total rows, per-tier
  counts, embedder status with `ready` / `disabled` / `dim-mismatch`
  pill, schema `user_version`).
- `GET /memory/recent?limit=50` drives the recent-rows table.
- `GET /memory/search?q=&k=` drives the search card.
- `POST /memory/save` is wired to a collapsed `<details>` "Save a new
  memory" card.
- `POST /memory/forget/<id>` is wired to a per-row `×` button gated
  by `confirm()`.

Auto-refresh is folded into the main 5s `/state.json` poller — when the
Memory tab is the active section, `refresh()` also calls
`refreshMemoryTab()`. Other tabs do not poll the memory routes, so the
SQLite mutex stays cold for users who never open the tab.

Anchor routing: `#memory` selects the tab on load and on `hashchange`.
The CLI verb `clud memory ui` (issue #262) opens the dashboard at
`http://127.0.0.1:<port>/#memory` so users land directly on the tab.

Tests:

- Rust `daemon::http::tests::dashboard_html_contains_memory_tab_markup`
  asserts the bundled HTML carries the tab + endpoint refs.
- Rust `daemon::http::tests::memory_routes_serve_realistic_payloads_for_dashboard`
  pushes a row through `SqliteStore::insert` and asserts
  `/memory/recent` returns the field shape the dashboard reads.
- Python `tests/test_memory_dashboard.py` exercises both the served
  HTML and `/memory/stats` against a real daemon process.

## Launch-setup integration (#265)

The opt-in surface for native memory is the third row of the launch-setup
scope selector — `Globally + clud memory (recommended)`. Selecting it runs
the existing `Global` actions (bundled skills, drift skills, hook timeout
normalization) AND two new actions registered for the selected backend only:

- `MemoryMcpRegistrationAction` — upserts the `clud-memory` MCP block into
  `~/.claude.json` (Claude) or `~/.codex/config.toml` (Codex).
- `MemoryHookRegistrationAction` — upserts the four `clud hook <verb>`
  entries into `~/.claude/settings.json` (Claude) or `~/.codex/hooks.json`
  (Codex).

Both actions live in `crates/clud-bin/src/launch_setup.rs` and delegate to
the idempotent upsert helpers in
[`crates/clud-bin/src/memory/mcp_config.rs`](../../crates/clud-bin/src/memory/mcp_config.rs):
`ensure_claude_mcp` / `ensure_codex_mcp` / `ensure_claude_hooks` /
`ensure_codex_hooks` (plus the four symmetric `remove_*` helpers used by
`clud memory uninstall` once that verb lands). Each helper acquires an
advisory `~/.clud/memory-{backend}-{kind}.lock` lock, parses the existing
config with `serde_json` (JSON) or `toml_edit` (TOML, so user comments
survive), upserts the managed block, and writes atomically via a temp
file + rename in the same directory. JSON entries carry a sibling
`_clud_managed: true` field; TOML entries carry a `# managed-by: clud-memory`
lead comment. An existing `clud-memory` key without the managed marker is
treated as user-defined: the helper returns `Error::UserDefined { path, key }`,
the action surfaces a `[clud] note: refusing to ...` line and continues without
writing.

The selector default flips based on whether memory is already registered
(`mcp_config::memory_already_registered(home)` — one stat + one parse), so a
fresh install opens on row 2 (the recommended row) and a repeat launch opens on
row 0. See
[`docs/architecture/launch-setup.md`](launch-setup.md) for the row-level
details and [DD-023](../DESIGN_DECISIONS.md#dd-023-scope-selector-third-row-enables-memory-mcphooks-in-one-action-refuse-to-clobber-semantics)
for the rationale on refuse-to-clobber semantics.

A `--dry-run` flag on `run_setup_at_with_options` emits a four-file preview
without touching disk; the same flag is exposed to users via
`clud --setup --dry-run` (no writes; prints the file paths and entries that
would be added).

## Sibling sub-issues (META #255)

- ~~Embeddings (local + remote + Windows-ARM strategy)~~ — #257.
- ~~Tier lifecycle (Working / Episodic / Semantic + decay + promotion)~~ — #258.
- ~~Daemon integration (lifecycle, consolidation timer, HTTP route stubs)~~ — #261.
- ~~MCP server in daemon + `clud mcp` stdio bridge~~ — #259.
- ~~Hook subcommands~~ — #260 (this PR).
- `clud memory search` / `save` CLI verbs (including
  `clud memory branch-isolate`) — #262.
- ~~Dashboard JS for the `/memory/*` routes~~ — #263.
- Cross-process persistence test.
- Knowledge graph (deferred past v1).

Each sub-issue lands on top of the public surface this PR exposes from
[`memory/mod.rs`](../../crates/clud-bin/src/memory/mod.rs); none of
them need to touch the storage layer itself.
