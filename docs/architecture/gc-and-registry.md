# GC & Registry

`clud` maintains two separate `redb` databases for two separate concerns: a per-launch
**session cap** that bounds how many sibling `clud` processes may run concurrently, and a
**tracked-entry GC** that owns the lifecycle of `.claude/worktrees/agent-*`, `.extern-repos/*`,
and known sibling temp-clone directories across repos and across invocations. The two stores live
in different files, on different code paths,
with different ownership models — they have nothing to do with each other besides both being
`redb`-backed, and confusing them is the single fastest way to wedge the system. This doc covers
both, plus the worktree scanner that feeds the GC store and the unrelated `--clean-worktrees`
subcommand.

## Two redb databases

| File | Concern | Ownership | Lifetime |
|---|---|---|---|
| `sessions.redb` (POSIX: `$XDG_STATE_HOME/clud/`; Windows: `%LOCALAPPDATA%\clud\`) | Per-launch session cap | File-lock serialized via `sessions.lock` | Opened for ms at startup and again at shutdown; never across the session |
| `~/.clud/data.redb` | Tracked-entry GC (`agent-*` worktrees, extern repos, sibling temp clones) | Single-owner: the always-on `clud __daemon` process owns it via the in-process `daemon/gc_service.rs` registry-worker thread; everyone else uses JSON-over-TCP | Opened once at daemon startup; lives until the daemon idle-shuts |

Why two files? The session cap is a *guardrail* — it needs the simplest possible "open, decide,
close" semantics so a crashed `clud` can never deadlock the next one. The tracked-entry GC is a
*long-running registry* with reconcile/list/purge operations, concurrent insert traffic from the
scanner thread, and a CLI surface that benefits from being able to ask "what's tracked?" without
re-walking every repo.

Splitting them lets each pick the right concurrency model. Mixing them would force the daemon to
also be in the startup-cap critical path, which is a recipe for fork-bomb-with-a-twist when the
daemon hangs.

Both files use the redb invariant **one writer per process** (`flock` on POSIX, `LockFileEx` on
Windows). The two architectures below are different solutions to that one constraint.

## Session cap registry

Background: issue #73 — a buggy test spawned 100+ console windows from a single terminal. The cap
is a hard guardrail against that class of mistake.

The schema is a single `redb` table `sessions` keyed by `pid: u32` → JSON-serialized `SessionRow`
(`crates/clud-bin/src/session_registry.rs:94`, `crates/clud-bin/src/session_registry.rs:103`). A
sibling `meta` table records `schema_version` (`crates/clud-bin/src/session_registry.rs:97`).

Lifecycle on each `clud` launch:

1. **Acquire** the cross-process advisory lock at `sessions.lock` next to `sessions.redb`
   (`crates/clud-bin/src/session_registry.rs:613`). Blocks until exclusive.
2. **Open** the redb file (`crates/clud-bin/src/session_registry.rs:402`).
3. **GC** dead rows whose PID no longer names a live process, using `OsLivenessProbe`
   (`crates/clud-bin/src/session_registry.rs:471`). On Windows the probe is
   `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION) + GetExitCodeProcess`; on POSIX it is
   `kill(pid, 0)`.
4. **Check** the cap (`crates/clud-bin/src/session_registry.rs:512`). Three outcomes: `Allow`,
   `Warn(n)` (at or above `cap/2`), or `Refuse(n)` (at or above cap).
5. **Register** this PID via `register_self` if not refused
   (`crates/clud-bin/src/session_registry.rs:520`). On `Refuse`, the row is **not** inserted —
   the caller exits, and inserting would inflate the count for the next sibling.
6. **Drop** the redb handle, then **release** the lock. The lock is held for a few ms — not for
   the lifetime of the session.

Shutdown re-acquires the lock briefly and removes the row via `unregister`
(`crates/clud-bin/src/session_registry.rs:546`). A crashed `clud` leaves a stale row that the
next startup's GC pass cleans up.

The whole startup sequence is wrapped by `run_startup_under_lock`
(`crates/clud-bin/src/session_registry.rs:677`) and the shutdown counterpart by
`run_shutdown_under_lock` (`crates/clud-bin/src/session_registry.rs:713`). The lock-then-redb
ordering is important: redb's file lock is released when its `Drop` runs, and we want that to
happen *before* the `sessions.lock` releases so the next sibling can open redb the instant it
acquires the advisory lock.

**Env overrides:**

- `CLUD_MAX_INSTANCES` (default 64; `0` disables the cap entirely;
  `crates/clud-bin/src/session_registry.rs:70`).
- `CLUD_WARN_INSTANCES` (default `cap/2`).
- `CLUD_SESSION_DB`, `CLUD_SESSION_LOCK` — test overrides for the file paths.

## Clud daemon (single-owner data.redb)

`~/.clud/data.redb` is held by **exactly one process**: the always-on `clud __daemon` subprocess.
Issue #135 Phase 1 originally introduced this as a separate `gc_daemon`; [DD-012] folded it
into the existing session daemon so there's one daemon per user that hosts both `--detach` /
`attach` / `list` / etc. and the GC registry. The single-owner-of-redb invariant is unchanged —
only the owning process identity moved.

The merged daemon's GC half:

1. Binds a loopback TCP port on `127.0.0.1:0` (kernel-assigned, shared with session-management
   IPC).
2. Spawns one **registry worker thread** that owns the `Registry` handle and is the sole
   reader/writer of the redb file (`crates/clud-bin/src/daemon/gc_service.rs:spawn_registry_worker`).
   The session-management half of the daemon runs in the same process but on different threads;
   nothing else touches the redb handle.
3. Atomically writes `(pid, port)` JSON to `~/.clud/state/daemon.json`
   (`crates/clud-bin/src/daemon/server.rs`).
4. Serves connections forever: accept thread → per-connection thread →
   `mpsc::Sender<GcRequestMsg>` → worker thread → reply on a per-request `mpsc::sync_channel(1)`
   (`crates/clud-bin/src/daemon/server.rs::dispatch_gc_op`).

Wire protocol is JSON-over-loopback-TCP, one request per connection. The session daemon's
existing `DaemonRequest`/`DaemonResponse` enum gained two variants:
`DaemonRequest::Gc { payload: GcOp }` and `DaemonResponse::Gc { reply: GcReply }`. Inside,
`GcOp` carries `list` / `purge` / `reconcile` / `insert` (`crates/clud-bin/src/daemon/types.rs`);
`GcReply` carries `list_ok` / `purge_ok` / `reconcile_ok` / `insert_ok` / `error`.

`ensure_daemon(state_dir)` (`crates/clud-bin/src/daemon/client.rs`) is the idempotent bringup
entry point, called from `main.rs` on every clud invocation. It reads `daemon.json`, probes the
PID, and if alive + accepting TCP, returns. Otherwise it acquires `<state_dir>/daemon.lock`
(issue #138 — serializes concurrent bringup so two `clud` startups don't both spawn a daemon and
race on the TCP bind), **re-probes** under the lock, and only spawns if still absent.

Spawn is `clud __daemon --state-dir <state_dir>` detached via `trampoline::spawn_detached_self`
with `invisible_helper_creationflags()` on Windows. Caller polls up to 5 seconds for the info
file plus a successful TCP connect.

[DD-012]: ../DESIGN_DECISIONS.md#dd-012-one-always-on-daemon-hosts-both-session-ops-and-the-gc-registry

## GC subcommands

All three subcommands are thin IPC clients against the daemon. `--no-daemon` (or
`CLUD_NO_DAEMON=1`) is **an error**, not a fallback — there is no read-only path in v1
(`crates/clud-bin/src/gc.rs:594`).

- **`clud gc list [--json]`** — `cmd_list` (`crates/clud-bin/src/gc.rs`) calls
  `daemon::gc_client_list`, prints a table (or JSON). Each row reports
  `kind / age / agent_id / branch / path` plus a `live_locked` flag computed by the daemon
  by parsing `git worktree list --porcelain` and extracting `(pid <N>)` from any
  `locked <reason>` line.
- **`clud gc purge [--duration 7d] [--kind worktree] [--dry-run] [--yes]`** — `cmd_purge`
  pre-validates the duration string locally, then calls `daemon::gc_client_purge`. The daemon
  selects candidates (all rows, or rows older than `now - duration`), partitions out
  live-locked worktrees or live session CWD ancestors, and:
  - `--dry-run` reports `removed` (the number that *would* be deleted) plus `skipped`. Replies
    `GcReply::PurgeOk { removed, skipped }` synchronously.
  - Non-dry-run **bulk** purge fans every purgeable entry out to the daemon's parallel purge pool
    (capped by `CLUD_GC_PURGE_CONCURRENCY`, default `min(num_cpus, 8)`) and returns
    immediately with `GcReply::PurgeStarted { dispatched, skipped }`. Each pool thread runs
    `remove_dir_all` / `git worktree remove --force` independently; on completion it sends a
    `RegistryMsg::PurgeCompletion(..)` back to the registry worker, which drops the matching
    redb row asynchronously. The redb writer never blocks on filesystem work — see #268.
  - Per-row delete (`GcOp::DeleteById`, dashboard "delete" button) stays on the synchronous path
    and replies `GcReply::PurgeOk { removed, skipped }` — exactly one entry, fast.
  Worktree rows use `git worktree remove --force`; if git fails or reports success while the
  directory survives, clud falls back to direct removal plus `git worktree prune`. Bare
  `clud gc purge` (no `--duration`) is interactive and asks for `y/N` confirmation unless
  `--yes`. Stale rows during/after a partial purge are tolerated — eventual consistency, not
  transactional; the next purge or list reconciles.
- **`clud gc reconcile`** — `cmd_reconcile` calls `daemon::gc_client_reconcile` with the
  current repo root. The daemon walks `<repo>/.claude/worktrees/`, `<repo>/.extern-repos/`, and
  conservative sibling temp-clone names next to the repo. Returns the count of new rows.

Bare `clud gc` (no subcommand) prints help and exits 0 without contacting the daemon
(`crates/clud-bin/src/gc.rs:591`).

## Worktree scanner

`WorktreeScanner` (`crates/clud-bin/src/gc.rs`) is a polling thread spawned at clud startup
from `main.rs` via `WorktreeScanner::maybe_spawn*()`. It walks `<repo>/.claude/worktrees/`,
`<repo>/.extern-repos/`, and conservative sibling temp-clone names every ~2 seconds and sends
`gc.insert` IPC ops for matching immediate subdirs it sees. The scanner is
**insert-only**: rows that already exist are no-ops (`Registry::insert_if_new` at
`crates/clud-bin/src/gc.rs:198`), so there is no per-cycle write churn.

Sleep is **chunked** — 20 × 100ms = ~2s total, but cancellable within 100ms — so Ctrl+C teardown
does not block for two seconds waiting for the next iteration (`crates/clud-bin/src/gc.rs:568`).

Cancellation is cooperative via `Arc<AtomicBool>`. `Drop` (`crates/clud-bin/src/gc.rs:506`) joins
the thread; the startup-side guard `_scanner_guard` in `main.rs` triggers this on normal
shutdown.

If the daemon is unreachable, the scanner logs once (debug level, gated by
`CLUD_GC_SCANNER_VERBOSE`) and **stops trying for the rest of the session**. It does not retry
on a backoff. This is intentional: in single-user CI/dev contexts there's no daemon to contact,
and quiet failure is preferable to a 2s-per-cycle stream of error logs.

## `--clean-worktrees`

`--clean-worktrees` (`crates/clud-bin/src/worktrees.rs`) is **unrelated to the GC store**. It is
a one-shot CLI flag for cleaning up git worktrees in the current repo, with no `redb`
involvement.

It enumerates via `git worktree list --porcelain`, classifies each entry as
`clean / dirty / unpushed / no-upstream / branch-gone` (`crates/clud-bin/src/worktrees.rs:53`),
and removes those that are *safe* — clean AND (older than `--stale-after` OR upstream `[gone]`).
`--force` widens the safe set to include `dirty` and `unpushed`.

Locked worktrees with fresh live/unknown/dead lock reasons are skipped. Once a lock exceeds
`CLUD_GC_LOCKED_HARD_AGE_DAYS` (default 7), `--clean-worktrees` presumes it is orphaned and lets
the entry pass through the same clean/dirty/unpushed/no-upstream/force rules as an unlocked
worktree. `--dry-run` is a faithful preview: nothing is mutated until the actual verified
worktree removal path.

The GC daemon does borrow `parse_worktree_porcelain` and the "extract pid from locked-reason"
helper from this module to compute the `live_locked` flag on `gc.list` output, but the two flows
are otherwise independent.

## Key types

Session cap:

- `SessionRegistry` (`crates/clud-bin/src/session_registry.rs:358`) — open redb handle plus
  own-pid + liveness probe.
- `CapConfig`, `CapDecision` (`crates/clud-bin/src/session_registry.rs:171`,
  `crates/clud-bin/src/session_registry.rs:191`) — pure cap-check inputs/outputs.
- `SessionInfo`, `SessionRow` (`crates/clud-bin/src/session_registry.rs:206`,
  `crates/clud-bin/src/session_registry.rs:103`) — public + on-disk row shapes.
- `LockGuard` (`crates/clud-bin/src/session_registry.rs:602`) — RAII for `sessions.lock`.

GC store / daemon:

- `Registry` (`crates/clud-bin/src/gc.rs`) — owns the redb handle inside the daemon's
  registry-worker thread.
- `TrackedEntry`, `InsertInput` (`crates/clud-bin/src/gc.rs`) — public row + insert-input
  shapes.
- `WorktreeScanner` (`crates/clud-bin/src/gc.rs`) — polling thread that talks IPC to the
  daemon.
- `DaemonInfo` (`crates/clud-bin/src/daemon/types.rs`) — info-file shape (pid + port) shared
  with the session-management half of the daemon.
- `ListRow` (`crates/clud-bin/src/daemon/types.rs`) — public JSON row shape returned by
  `gc.list` and serialized by `clud gc list --json`.
- `GcOp`, `GcReply` (`crates/clud-bin/src/daemon/types.rs`) — IPC payload enums carried inside
  `DaemonRequest::Gc` / `DaemonResponse::Gc`. `GcReply::PurgeOk { removed, skipped }` is the
  synchronous outcome (dry-run + DeleteById); `GcReply::PurgeStarted { dispatched, skipped }`
  is the bulk-purge fan-out outcome.
- `RegistryMsg`, `GcRequestMsg`, `PurgeCompletion`, `PurgeJob` (`crates/clud-bin/src/daemon/gc_service.rs`)
  — the worker's internal mpsc carries both client ops (`RegistryMsg::Op(GcRequestMsg)`) and
  fire-and-forget purge-pool callbacks (`RegistryMsg::PurgeCompletion`). The pool's job queue
  is `mpsc::Sender<PurgeJob>`.

`--clean-worktrees`:

- `WorktreeEntry`, `WorktreeStatus` (`crates/clud-bin/src/worktrees.rs:30`,
  `crates/clud-bin/src/worktrees.rs:53`).
- `CleanOptions` (`crates/clud-bin/src/worktrees.rs:91`).

## Failure modes

- **Daemon crash mid-request.** Client's TCP `read_line` returns 0 bytes; `gc_client_*`
  surfaces as `io::Error`. CLI prints `error: <op> failed: <msg>` and exits 1. The next
  `ensure_daemon()` from any clud process re-spawns the daemon. The worker's
  `recv_timeout(WORKER_REPLY_TIMEOUT)` in `dispatch_gc_op`
  (`crates/clud-bin/src/daemon/server.rs`) prevents a wedged worker from hanging the accept
  thread indefinitely.

- **Bringup race.** Two `clud` startups call `ensure_daemon()` simultaneously. Both see no
  daemon. Both try to acquire `daemon.lock`. The winner spawns; the loser blocks, then
  re-probes under the lock, finds the daemon, and returns. Issue #138.

- **`sessions.redb` lock contention.** N concurrent `clud` launches serialize on `sessions.lock`,
  never on the redb file lock. The lock is held for ~ms per launch; even hundreds of
  simultaneous spawns drain in well under a second on the host.

- **`sessions.redb` corruption.** `Database::create` returns a `redb::DatabaseError`;
  `run_startup_under_lock` surfaces it; `main.rs` logs and skips the cap check rather than
  refusing to launch (the cap is best-effort safety, not a hard requirement). The user can `rm`
  the file to recover.

- **Scanner thread panic.** The scanner thread is independent of the main launch path; a panic
  logs to stderr but does not affect the running session. `Drop::cancel` joins, so a panic in
  the worker also won't leak the thread.

- **Worktree dir gone mid-scan.** `read_dir` returns `NotFound` → `scan_once_via_ipc` treats as
  empty (`crates/clud-bin/src/gc.rs:518`). Individual `agent-*` dirs disappearing between
  `read_dir` and the `gc.insert` IPC are benign: the daemon will accept the insert, and a future
  `gc purge` (or a `git worktree remove` from outside clud) cleans the row.

- **PID reuse.** The session-cap registry's `Drop` only deletes its row if `register_self` was
  called successfully (`crates/clud-bin/src/session_registry.rs:565`), so an early-aborted `clud`
  cannot clobber a sibling that happened to inherit its PID via POSIX PID reuse. `unregister`
  clears the flag too, for the same reason.

- **`CLUD_NO_DAEMON=1` or `--no-daemon`.** All `clud gc *` subcommands exit 2 with "gc
  operations require the clud daemon; remove --no-daemon". The scanner's IPC fails silently
  and the thread idles for the rest of the session. The session cap is unaffected (it doesn't
  use the daemon). The always-on auto-spawn from `main.rs` is also skipped.

## See also

- [`daemon-ipc.md`](daemon-ipc.md) — the always-on `clud __daemon` hosts both interactive
  session ops (`Create` / `Session` / `Terminate`) and GC ops (`Gc { payload }`) on the same
  loopback TCP listener.
- [`../DESIGN_DECISIONS.md`](../DESIGN_DECISIONS.md) — DD-006 introduced the "redb is
  single-owner" rule; DD-012 records the merge of `gc_daemon` into the session daemon while
  preserving that invariant.
