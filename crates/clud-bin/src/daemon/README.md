# daemon/

Always-on background service for every `clud` invocation (issue #135). One long-lived daemon process per user owns two distinct concerns served from the same loopback TCP listener:

1. **Session manager** — spawns per-session worker subprocesses for `clud --detach`, `clud attach`, `clud list`, `clud kill`, `clud logs`, and `clud loop --repeat`. Each worker owns one backend (`claude` or `codex`) running under a PTY or a captured subprocess, persists snapshots + an append-only log to disk, and brokers attach/detach from interactive clients.
2. **GC service** — single-owner of `~/.clud/data.redb`. All `clud gc *` IPC ops route to a dedicated registry-worker thread (`gc_service.rs`); the worker is the sole reader/writer of the redb file, eliminating multi-process locking races (issue #138).

Foreground interactive launches still use the direct runner by default — the daemon hosts the centralized PTY path only when explicitly opted in (`--detach`, `--detachable`, `--experimental-daemon-centralized`, repeat jobs). See [docs/architecture/daemon-ipc.md](../../../../docs/architecture/daemon-ipc.md) for the wire protocol, attach flow, snapshot/log persistence, and failure modes. This README is the per-file inventory.

Internal helper subcommands `__daemon` and `__worker` re-enter the same binary in their respective roles.

## Files

- `mod.rs` — module root. Re-exports the public surface: `ensure_daemon`, `default_state_dir`, `ENV_NO_DAEMON`, `ListRow`, `gc_client_{list,purge,reconcile,insert}` plus the existing `experimental_enabled` / `handle_special_command` / `run_centralized_session`.
- `entry.rs` — public dispatch: feature-flag check, routing for `attach`/`kill`/`list`/`logs`/`__daemon`/`__worker`, and the main `Create` request for normal sessions.
- `types.rs` — shared structs, enums, env-var keys, and constants (`SessionSnapshot`, `WorkerLaunchSpec`, `DaemonRequest`/`Response`, `GcOp`/`GcReply`, `ListRow`, `WorkerClientMessage`/`ServerMessage`, `SessionRuntime`, `RawTerminalGuard`, `ENV_NO_DAEMON`).
- `paths.rs` — filesystem layout helpers under the daemon state dir (`default_state_dir` → `~/.clud/state`, `daemon.json`, `daemon.lock` bringup serialization, `sessions/`, `specs/`, `logs/`).
- `client.rs` — client-side daemon RPC: `ensure_daemon` (idempotent fs4-locked auto-spawn), `send_daemon_request`, `request_session_termination`, `gc_client_*` IPC wrappers for the four `clud gc` ops, stale-state cleanup.
- `server.rs` — daemon-process entry: binds the loopback listener, spawns the GC registry worker, accepts `Create`/`Session`/`Terminate`/`Gc` requests, spawns worker subprocesses, reaps them.
- `gc_service.rs` — single-owner registry worker thread (issue #135): opens `~/.clud/data.redb` once, serializes every `gc.*` op through an `mpsc::Receiver<GcRequestMsg>`. Replaces the standalone `gc_daemon` process that shipped in Phase 1.
- `worker.rs` — worker-process entry: starts the backend (subprocess or PTY), serves attach connections, runs the repeat-job loop.
- `worker_shared.rs` — per-worker shared state: snapshot, in-memory backlog, optional `TerminalCapture` for PTY attach-replay, log file rotation, single-client attach gate.
- `attach.rs` — interactive client-side attach loop: handshake, raw-terminal keyboard forwarding, Ctrl-C → background-prompt flow, exit-code propagation.
- `commands.rs` — implementations of `clud kill`, `clud list`, `clud logs` (including pm2-style tail/follow with rotation handling).
- `sessions.rs` — snapshot discovery + filtering: `resolve_session_id` (exact/name/prefix), `most_recent_session[_any]`, `list_background_sessions`, `list_attachable_sessions`.
- `keys.rs` — `crossterm` `KeyEvent` → terminal byte sequence translator used by interactive attach.
- `io_helpers.rs` — JSON read/write over TCP + atomic file writes, session-id generator, terminal-size probe, `--backlog-size` / `CLUD_BACKLOG_BYTES` parsing.
- `wire_prost.rs` - prost v1 foundation for the daemon wire: generated `clud.v1` types, CLUD/CLJS payload protocol discriminators, encode/decode helpers, JSON-compatibility tests, the default prost daemon RPC path, and the `CLUD_DAEMON_WIRE=json` legacy fallback.
- `process_utils.rs` — `pid_is_alive`, `signal_process_tree`, `descendant_pids` via `sysinfo`.

## Key items

- `pub fn experimental_enabled(&Args) -> bool` — `entry.rs:21`
- `pub fn handle_special_command(&Args, &AtomicBool) -> Option<i32>` — `entry.rs:38`
- `pub fn run_centralized_session(&Args, &LaunchPlan, &AtomicBool) -> i32` — `entry.rs:144`
- `enum DaemonRequest { Create, Session, Terminate }` — `types.rs:103`
- `enum DaemonResponse { Created, Session, Terminated, Error }` — `types.rs:111`
- `enum WorkerClientMessage { Attach, Input, Resize, Interrupt }` — `types.rs:120`
- `enum WorkerServerMessage { Attached, Output, Exited, Error }` — `types.rs:129`
- `struct SessionSnapshot` — on-disk + wire session metadata — `types.rs:48`
- `struct WorkerLaunchSpec` — daemon→worker launch contract — `types.rs:77`
- `enum SessionRuntime { Subprocess, Pty }` — runtime handle abstraction — `types.rs:137`
- `enum SessionKind { Subprocess, Pty }` — `types.rs:36`
- `const ENV_FEATURE_FLAG = "CLUD_EXPERIMENTAL_DAEMON"` — `types.rs:17`
- `const ENV_STATE_DIR = "CLUD_DAEMON_STATE_DIR"` — `types.rs:18`
- `const DEFAULT_BACKLOG_LIMIT_BYTES = 256 KiB` — `types.rs:20`
- `const LOG_ROTATE_BYTES = 10 MiB` — `types.rs:28`
- `fn run_daemon(&Path) -> i32` — `server.rs:23`
- `fn run_worker(&Path, &str, u32, &Path) -> i32` — `worker.rs:28`
- `fn ensure_daemon(&Path) -> io::Result<()>` — `client.rs:18`
- `fn send_daemon_request(&Path, &DaemonRequest)` — `client.rs:51`
- `fn run_attach(&str, &Path, &AtomicBool) -> i32` — `attach.rs:26`
- `fn run_kill / run_list / run_logs` — `commands.rs:14`, `commands.rs:82`, `commands.rs:159`
- `fn resolve_session_id(&Path, &str)` — `sessions.rs:11`
- `struct WorkerShared` (+ `attach_client`, `push_output`, `broadcast_exit`, `evict_dead_client`, log rotation) — `worker_shared.rs:22`
- `fn translate_key_event(KeyEvent) -> KeyAction` — `keys.rs:5`
- `fn resolve_backlog_bytes(Option<&str>) -> Option<usize>` — `io_helpers.rs:77`
- `fn signal_process_tree(u32, Signal)` — `process_utils.rs:10`

## Used by

- `crates/clud-bin/src/main.rs` — sole external consumer; calls `experimental_enabled`, `handle_special_command`, and `run_centralized_session`.
- `crates/clud-bin/src/process_tree.rs` — doc-only cross-reference to `signal_process_tree`.
- Re-enters itself via the hidden `__daemon` / `__worker` subcommands defined in `crates/clud-bin/src/args.rs`.
