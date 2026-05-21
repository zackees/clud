# GC & Registry

`clud` maintains two separate `redb` databases for two separate concerns: a per-launch
**session cap** that bounds how many sibling `clud` processes may run concurrently, and a
**tracked-entry GC** that owns the lifecycle of `.claude/worktrees/agent-*` directories across
repos and across invocations. The two stores live in different files, on different code paths,
with different ownership models — they have nothing to do with each other besides both being
`redb`-backed, and confusing them is the single fastest way to wedge the system. This doc covers
both, plus the worktree scanner that feeds the GC store and the unrelated `--clean-worktrees`
subcommand.

## Two redb databases

| File | Concern | Ownership | Lifetime |
|---|---|---|---|
| `sessions.redb` (POSIX: `$XDG_STATE_HOME/clud/`; Windows: `%LOCALAPPDATA%\clud\`) | Per-launch session cap | File-lock serialized via `sessions.lock` | Opened for ms at startup and again at shutdown; never across the session |
| `~/.clud/data.redb` | Tracked-entry GC (`agent-*` worktrees) | Single-owner `clud __gc-daemon` process; everyone else uses JSON-over-TCP | Opened once by the daemon; lives until the daemon dies |

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

## GC daemon (single-owner data.redb)

`~/.clud/data.redb` is held by **exactly one process**: the `clud __gc-daemon` subprocess. Issue
#135 Phase 1 introduced this; the design rationale (`crates/clud-bin/src/gc_daemon.rs:13`) is
that redb's locking is intra-process and any attempt to coordinate multi-process access via the
file lock was unreliable in practice — startup races, partial-write windows, and the cost of
opening/closing redb on every CLI invocation all pushed us toward funneling everything through
one owner.

The daemon process:

1. Binds a loopback TCP port on `127.0.0.1:0` (kernel-assigned).
2. Spawns one **registry worker thread** that owns the `Registry` handle and is the sole
   reader/writer of the redb file (`crates/clud-bin/src/gc_daemon.rs:224`).
3. Atomically writes `(pid, port)` JSON to `~/.clud/state/gc-daemon.info`
   (`crates/clud-bin/src/gc_daemon.rs:119`).
4. Serves connections forever: accept thread → per-connection thread →
   `mpsc::Sender<GcRequestMsg>` → worker thread → reply on a per-request `mpsc::sync_channel(1)`
   (`crates/clud-bin/src/gc_daemon.rs:469`).

Wire protocol is JSON-over-loopback-TCP, one request per connection, versioned envelope
`{"v": 1, "op": "...", ...}`. Operations: `gc.list`, `gc.purge`, `gc.reconcile`, `gc.insert`
(`crates/clud-bin/src/gc_daemon.rs:144`). Replies are `gc.list.ok`, `gc.purge.ok`,
`gc.reconcile.ok`, `gc.insert.ok`, `gc.insert.skipped`, or `error`
(`crates/clud-bin/src/gc_daemon.rs:186`).

`ensure_running()` (`crates/clud-bin/src/gc_daemon.rs:547`) is the idempotent bringup entry
point. It reads `gc-daemon.info`, probes the PID, and if alive + accepting TCP, returns.
Otherwise it acquires `<state_dir>/gc-daemon.lock` (issue #138 — serializes concurrent bringup so
two `clud` startups don't both spawn a daemon and race on the TCP bind), **re-probes** under the
lock, and only spawns if still absent.

Spawn is `clud __gc-daemon --state-dir <state_dir>` detached via
`trampoline::spawn_detached_self` with `invisible_helper_creationflags()` on Windows. Caller
polls up to 5 seconds for the info file plus a successful TCP connect.

## GC subcommands

All three subcommands are thin IPC clients against the daemon. `--no-daemon` (or
`CLUD_NO_DAEMON=1`) is **an error**, not a fallback — there is no read-only path in v1
(`crates/clud-bin/src/gc.rs:594`).

- **`clud gc list [--json]`** — `cmd_list` (`crates/clud-bin/src/gc.rs:631`) sends `gc.list`,
  prints a table (or JSON). Each row reports `kind / age / agent_id / branch / path` plus a
  `live_locked` flag computed by the daemon by parsing `git worktree list --porcelain` and
  extracting `(pid <N>)` from any `locked <reason>` line.
- **`clud gc purge [--duration 7d] [--kind worktree] [--dry-run] [--yes]`** — `cmd_purge`
  (`crates/clud-bin/src/gc.rs:676`) pre-validates the duration string locally, then sends
  `gc.purge`. The daemon selects candidates (all rows, or rows older than `now - duration`),
  partitions out live-locked worktrees, and either reports the count (dry-run) or runs `git
  worktree remove --force` (falling back to `remove_dir_all` if git refuses) and deletes the
  row. Bare `clud gc purge` (no `--duration`) is interactive and asks for `y/N` confirmation
  unless `--yes`.
- **`clud gc reconcile`** — `cmd_reconcile` (`crates/clud-bin/src/gc.rs:653`) sends
  `gc.reconcile` with the current repo root. The daemon walks `<repo>/.claude/worktrees/` and
  inserts any agent-* subdir that isn't already tracked. Returns the count of new rows.

Bare `clud gc` (no subcommand) prints help and exits 0 without contacting the daemon
(`crates/clud-bin/src/gc.rs:591`).

## Worktree scanner

`WorktreeScanner` (`crates/clud-bin/src/gc.rs:459`) is a polling thread spawned at clud startup
from `main.rs:296` via `WorktreeScanner::maybe_spawn()`. It walks `<repo>/.claude/worktrees/`
every ~2 seconds and sends `gc.insert` IPC ops for any `agent-*` subdir it sees. The scanner is
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

**Locked worktrees are never removed, even with `--force`**
(`crates/clud-bin/src/worktrees.rs:268`). `--dry-run` is a faithful preview: nothing is mutated
until the actual `git worktree remove` invocation.

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

- `Registry` (`crates/clud-bin/src/gc.rs:149`) — owns the redb handle inside the daemon worker
  thread.
- `TrackedEntry`, `InsertInput` (`crates/clud-bin/src/gc.rs:119`,
  `crates/clud-bin/src/gc.rs:133`) — public row + insert-input shapes.
- `WorktreeScanner` (`crates/clud-bin/src/gc.rs:459`) — polling thread.
- `DaemonInfo`, `DaemonHandle` (`crates/clud-bin/src/gc_daemon.rs:74`,
  `crates/clud-bin/src/gc_daemon.rs:532`) — info-file shape + ensure-running return value.
- `ListRow` (`crates/clud-bin/src/gc_daemon.rs:205`) — public JSON row.

`--clean-worktrees`:

- `WorktreeEntry`, `WorktreeStatus` (`crates/clud-bin/src/worktrees.rs:30`,
  `crates/clud-bin/src/worktrees.rs:53`).
- `CleanOptions` (`crates/clud-bin/src/worktrees.rs:91`).

## Failure modes

- **Daemon crash mid-request.** Client's TCP `read_line` returns 0 bytes; `client_*` surfaces
  as `io::Error`. CLI prints `error: <op> failed: <msg>` and exits 1. The next `ensure_running()`
  from any clud process re-spawns the daemon. The worker's `recv_timeout(30s)` in
  `handle_connection` (`crates/clud-bin/src/gc_daemon.rs:508`) prevents a wedged worker from
  hanging the accept thread indefinitely.

- **Bringup race.** Two `clud` startups call `ensure_running()` simultaneously. Both see no
  daemon. Both try to acquire `gc-daemon.lock`. The winner spawns; the loser blocks, then
  re-probes under the lock, finds the daemon, and returns its handle. Issue #138.

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

- **`CLUD_NO_DAEMON=1`.** All `clud gc *` subcommands exit 2 with "gc operations require the GC
  daemon; remove --no-daemon". The scanner's IPC fails silently and the thread idles for the
  rest of the session. The session cap is unaffected (it doesn't use the daemon).

## See also

- [`daemon-ipc.md`](daemon-ipc.md) — the long-lived session daemon uses the same
  JSON-over-loopback-TCP pattern but is a *different* daemon process serving a different concern
  (interactive sessions, not GC). The GC daemon's protocol is private to this subsystem.
- [`../DESIGN_DECISIONS.md`](../DESIGN_DECISIONS.md) — DD-006 covers the "redb is single-owner"
  rule and the rationale for the daemon-fronted design over per-process file-lock coordination.
