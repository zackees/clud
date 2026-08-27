# Daemon IPC

The daemon is a long-lived, single-binary session manager that owns backgrounded `clud` runs and brokers `attach` / `list` / `kill` / `logs` / `--repeat`. Without it, every `clud` invocation would be its own foreground process tied to one terminal; with it, a user can `clud --detach -p ...` to spawn a backend (`claude` or `codex`) that survives terminal close, then later `clud attach <id>` to rejoin its PTY from a different TTY. The IPC layer is three lanes: a `running-process` **broker frame lane** (a named pipe on Windows, a Unix socket elsewhere) that daemon RPCs take by default, a loopback-TCP fallback for the same RPCs, and a loopback-TCP stream per attached client for the worker side. A second loopback listener serves the dashboard over HTTP. State lives under a per-user state directory and the daemon process re-enters the same `clud` binary as hidden `__daemon` and `__worker` subcommands.

## Component map

Three processes, one binary. All files in `crates/clud-bin/src/daemon/`.

**Daemon process** — at most one per state-dir. Spawned lazily by the first client that needs it, accepts loopback TCP, dispatches `Create` / `Session` / `Terminate`, spawns and reaps worker children.
- `server.rs` — listeners, request dispatch, worker spawn, reap thread.
- `rp_broker/` — the **default** RPC lane. `endpoint.rs` resolves the named
  pipe / Unix socket and owns `daemon-identity.json`; `frame_lane.rs` serves it;
  `mod.rs` is the client half. Disabled by `RUNNING_PROCESS_DISABLE=1`, which
  drops every caller back to the TCP lane.
- `http.rs` — the dashboard's own loopback HTTP listener, on a separate port
  recorded as `dashboard_port` in `daemon.json`.
- `paths.rs` — on-disk layout helpers under `<state_dir>/`.
- `process_utils.rs` — `pid_is_alive`, `signal_process_tree` (Term then Kill across the descendant tree via `sysinfo`).

**Worker process** — one per session. Owns the backend PTY or captured subprocess, persists `SessionSnapshot`, appends to the rotating log, brokers exactly one attached client at a time.
- `worker.rs` — main loop: bind worker port, start backend, accept attach connections, watchdog the daemon pid.
- `worker_shared.rs` — `WorkerShared`: snapshot, in-memory backlog, optional `TerminalCapture`, log file, single-client gate, dead-peer eviction.
- `types.rs` — wire types and runtime structs (`SessionRuntime`, `AttachedClient`, `RawTerminalGuard`).

**Client side** — invoked from every `clud` front-end and from every `clud attach` / `list` / `kill` / `logs`.
- `client.rs` — `ensure_daemon`, `send_daemon_request`, `request_session_termination`, `cleanup_stale_state`.
- `attach.rs` — interactive attach loop, raw-terminal keyboard forwarding, Ctrl+C → background-prompt flow, exit-code propagation.
- `commands.rs` — `clud kill`, `clud list`, `clud logs` (pm2-style tail / follow with rotation handling).
- `sessions.rs` — `resolve_session_id` (exact / name / unique prefix), `most_recent_session[_any]`, `list_attachable_sessions`.
- `keys.rs` — `crossterm::KeyEvent` → terminal byte sequences (Ctrl chords, arrow keys, F-keys).
- `entry.rs` — single dispatch point: feature-flag check, special-command routing, `run_centralized_session`.
- `io_helpers.rs` — JSON line read / write, atomic file writes, session-id generator, backlog-size parser.

## Process model

`clud` is one binary; the daemon and the worker are the same exe re-entered with hidden internal subcommands. The trampoline that detaches the daemon lives in `crate::trampoline`; client invocation is in `ensure_daemon` at `client.rs:43`.

1. A normal `clud` invocation calls `experimental_enabled` (`entry.rs:21`). Centralized routing is now forced on by `--detach` / `--detachable` / `--transcript <path>` / repeat-mode / `--experimental-daemon-centralized` / `CLUD_EXPERIMENTAL_DAEMON=1`. Piped invocations (`clud -p "x" | jq`, CI runners) stay on the direct runner unless one of those explicit daemon features is requested, so stdio framing is byte-identical for script automation. The user can opt out explicitly with `--no-daemon`. When centralized is active, the front-end shells out to `run_centralized_session` (`entry.rs:144`) instead of running the backend directly.
2. `run_centralized_session` calls `ensure_daemon` which probes the port in `daemon.json`; if no live daemon answers, it spawns `current_exe` with argv `["__daemon", "--state-dir", <path>]` via `trampoline::spawn_detached_self` (`client.rs:81`) and waits up to 5s for the new daemon to write `daemon.json` and accept a probe. A newer live daemon is never replaced by an older client: the client prints a yellow compatibility error and exits 1.
3. On a `Create` request, the daemon spawns `current_exe` again with `["__worker", "--state-dir", ..., "--session-id", ..., "--daemon-pid", ..., "--spec-file", ...]` (`server.rs:669`). The worker is launched with `Containment::Detached`, `StdinMode::Null`, `StderrMode::Stdout`, and on Windows uses `invisible_helper_creationflags` to suppress a conhost flash (issue #55).
4. Both internal subcommands are dispatched in `handle_special_command` (`entry.rs:38`): `Command::InternalDaemon` → `run_daemon` (`server.rs:56`), `Command::InternalWorker` → `run_worker` (`worker.rs:32`). The same dispatch handles all user-facing subcommands (`Attach`, `Kill`, `List`, `Logs`) so a single early-return covers every "not a normal run" path.

The hidden subcommands are declared in `crates/clud-bin/src/args.rs` and accept their state-dir / session-id / pid / spec-file as explicit flags rather than env vars, so a stuck worker shows up in `ps` with a self-describing argv.

### Safe idle retirement

The daemon is lazy rather than permanently resident. New settings seed
`daemon.idle_timeout_secs` to 900 seconds; a user may select another positive duration or set
it to `0` to leave automatic retirement disabled. Idle retirement is prevented by daemon workers,
foreground client leases, live dashboard/top polling, active RPC connections (including the broker
frame lane), and pending maintenance. `ensure_daemon` transparently starts a replacement on the
next normal invocation after an idle exit.

The daemon owns its HTTP telemetry endpoint only while it is live. An inherited telemetry URL is
therefore valid while the foreground client holding its lease remains alive; after that client is
gone, late telemetry delivery is best-effort and a producer must tolerate the endpoint being gone.

This policy deliberately follows authoritative ownership rather than an RPC-only or fixed
wall-clock timer: foreground work, detached workers, and maintenance can all remain useful without
continuous requests. Host process discovery and orphan-snapshot consolidation remain owned by #548.

## Wire protocol

### Three lanes

| Lane | Transport | Carries | Where |
|---|---|---|---|
| **Broker frame lane** (default) | named pipe (Windows) / Unix socket (elsewhere) | every `DaemonRequest` | `rp_broker/endpoint.rs:36`, `rp_broker/frame_lane.rs:153` |
| **Loopback TCP** (fallback) | `127.0.0.1:<port>` from `daemon.json` | the same `DaemonRequest`s | `client.rs:168` |
| **Worker attach** | `127.0.0.1:<worker_port>` | `WorkerClientMessage` / `WorkerServerMessage` | `worker.rs:32` |

Plus the **dashboard HTTP listener** (`http.rs:521`, spawned at `server.rs:142`),
a second loopback port recorded as `dashboard_port` in `daemon.json`. It is not
part of the RPC surface.

`send_daemon_request` (`client.rs:168`) tries the frame lane **first** and falls
through to TCP on any miss — `RUNNING_PROCESS_DISABLE=1`, no
`daemon-identity.json` sidecar, or a connect/wire failure. TCP therefore remains
the authoritative path in the sense that it always works, but it is not the
common one.

> **The named pipe is the default, not the rejected alternative.** An earlier
> revision of this doc cited `DESIGN_DECISIONS.md` for choosing "TCP+JSON over
> named pipes / Unix sockets". That is now backwards and produced at least one
> confident wrong answer (#692). See [DD-025](../DESIGN_DECISIONS.md) for the
> reversal.

### Connection lifetime is a client policy, not a protocol limit

The daemon's frame-lane `serve_connection` (`rp_broker/frame_lane.rs:236`) is an
unbounded loop: it multiplexes as many frames over one connection as the peer
sends. The **client** is what makes RPCs look one-shot — `try_send_via_frame_lane`
(`rp_broker/mod.rs:116`) adopts a `BrokerSession`, sends one request, reads one
reply, and drops it.

So a caller who wants a long-lived two-way channel (a subscription, a stream)
does **not** need the daemon rewritten. The TCP lane *is* one-request-per-connection
(`handle_daemon_connection`, `server.rs:307`); the frame lane is not.

### Encoding

Daemon RPCs default to framed prost. Legacy JSON remains available with
`CLUD_DAEMON_WIRE=json`: every message is a single line of UTF-8 JSON terminated
by `
` (`write_json_line`, `io_helpers.rs:25`). JSON enums are tagged with
`"op"` (serde `tag = "op"`, snake_case variants).

The prost foundation lives in `wire_prost.rs` and `proto/clud_v1.proto`. It
defines `CLUD` (`0x434c5544`) for prost payloads and `CLJS` (`0x434c4a53`) for
legacy JSON so the dispatcher routes by `Frame.payload_protocol` without changing
the JSON shapes. Unset `CLUD_DAEMON_WIRE` and `CLUD_DAEMON_WIRE=prost` send a
`CLUD-FRAME/1 <payload_protocol_hex> <base64_payload>` line envelope carrying the
v1 prost payload; the daemon replies in the format it received. Worker attach
streams use the same dispatcher after the attach request selects a format. The
live-daemon `ListLiveCwds` perf gate pins prost median RPC latency to no more
than 20% above the JSON median for that read-only path.

### Client → daemon

`DaemonRequest` (`types.rs:216`) — **all eleven variants**:

| `"op"` | Payload | Reply | Notes |
|---|---|---|---|
| `create` | `spec: WorkerLaunchSpec` (boxed) | `created { session }` | Daemon spawns the worker and waits up to 5s for snapshot + port-probe. |
| `session` | `session_id: String` | `session { session }` | Read-only fetch of the on-disk snapshot. |
| `list_live_cwds` | — | `live_cwds { paths }` | Canonicalized CWD per live session snapshot. |
| `terminate` | `session_id: String` | `terminated { session }` | Signals the worker tree (Term, 150 ms, Kill); marks `exit_code = 130`. |
| `interrupt` | `session_id`, `profile: CtrlCProfile` | `interrupted { session }` | |
| `adopt_kill` | `pids: Vec<u32>`, `reason: Option<String>` | `adopt_kill_ack { accepted }` | **Fire-and-forget.** Acked *before* the kill so the CLI's Ctrl+C-to-shell latency stays sub-100 ms. |
| `gc` | `payload: GcOp` | `gc { reply }` | Served by the registry worker thread (`gc_service.rs`). One variant wraps every `gc.*` op so wire format and dispatch share a definition. |
| `shutdown` | `client_version`, `expected_daemon { pid, start_time }` | `shutdown_ack { pid }` or `error` | `clud daemon stop/restart`. The daemon exits only when the caller is the same/newer version and names the exact running generation; legacy, older, and stale-generation requests are refused. |
| `reap_orphans` | — | `reap_orphans_ack { found, reaped }` | **Fire-and-forget**; sweep runs on a background thread, so the ack carries zeros except on the synchronous test path. See [process-reaping.md](process-reaping.md). |
| `metrics` | — | `metrics { pid, cpu_pct }` | Ambient status without scraping dashboard JSON. |
| `proc_snapshot` | `include_dead_since_ms: u64` | `proc_snapshot { snapshot }` | The daemon's most recent **cached** process-tree sample. The daemon must not enumerate synchronously for this request; callers get whatever the background sampler last wrote. This is the seam #687 builds on — it already exists. |

`DaemonResponse` (`types.rs:276`) is the twelve replies above plus
`error { message: String }`, the catch-all failure.

### Client → worker

`WorkerClientMessage` (`types.rs:557`):

| `"op"` | Payload | Notes |
|---|---|---|
| `attach` | `terminal: Option<TerminalCapabilities>`, `rows: Option<u16>`, `cols: Option<u16>` | Mandatory handshake; any other first message gets `error "expected attach handshake"` and the connection drops. |
| `input` | `data_b64: String`, `submit: bool` | Base64 bytes. `submit` is the "press Enter after this paste" hint forwarded to the PTY's `write_impl`. |
| `resize` | `rows: u16`, `cols: u16` | Forwarded to the PTY and to the server-side `TerminalCapture` parser. |
| `interrupt` | `profile: Option<CtrlCProfile>` | Subprocess gets `kill()`, PTY gets `send_interrupt_impl()`. |

### Worker → client

`WorkerServerMessage` (`types.rs:582`):

| `"op"` | Payload | Notes |
|---|---|---|
| `attached` | `session: Box<SessionSnapshot>` | Always first; the snapshot at attach time. |
| `output` | `data_b64: String` | Base64 chunks; on PTY sessions the first is the synthesized repaint. |
| `exited` | `exit_code: i32` | Terminal: the writer thread breaks and the client returns this code. |
| `error` | `message: String` | Either pre-handshake (no attach) or during attach (slot taken). |

Forward-compat: `SessionSnapshot` (`types.rs:80`) has `#[serde(default)]` on every non-essential field and `WorkerLaunchSpec.backlog_bytes` is `Option<usize>` (`types.rs:206`) precisely so older daemons can read spec files written by newer clients without crashing. Add fields the same way; do not rename or retag existing variants.

Provider/harness fields inside `WorkerLaunchSpec.plan` follow that same
additive rule. Their accessors fall back to the legacy backend field, and a
repeat worker receives the already-resolved values rather than consulting the
current settings file. The foreground bridge is not an RPC endpoint; see
[codex-via-claude.md](codex-via-claude.md) for its distinct ownership and
security boundary.

## Daemon lifecycle

`run_daemon` (`server.rs:56`):

1. `fs::create_dir_all(state_dir)`.
2. `cleanup_stale_state` (`client.rs:457`) — mark snapshots whose `worker_pid` is dead as `exit_code = Some(137)`, GC dangling spec files older than 10s (the grace window for slow worker startup), drop `daemon.json` if its pid is dead.
3. Bind the frame-lane endpoint (`start_frame_lane`, `rp_broker/frame_lane.rs:153`) and a `TcpListener` on `127.0.0.1:0`, then write `DaemonInfo { pid, port, dashboard_port }` to `daemon.json` (`server.rs:157`). The frame lane additionally writes `daemon-identity.json`, which is what lets a client skip the Hello handshake.
4. The TCP accept loop spawns one thread per connection running `handle_daemon_connection` (`server.rs:307`); that lane reads exactly one `DaemonRequest`, dispatches, writes one `DaemonResponse`, closes. The frame lane's `serve_connection` (`rp_broker/frame_lane.rs:236`) instead loops, serving frames until the peer hangs up.
5. Worker handles are stored in `HashMap<String, Arc<NativeProcess>>` and reaped by `reap_worker_when_done` (`server.rs:761`), which `wait()`s in a thread and removes the entry once the child exits.
6. There is no graceful shutdown — the daemon exits when the process is killed. State on disk is the source of truth; a restarted daemon picks up where the previous one left off (modulo the stale-state cleanup pass).

## Worker lifecycle

`run_worker` (`worker.rs:32`):

1. Load `WorkerLaunchSpec` from the spec file the daemon wrote. If `repeat_run_command` is set, divert to `run_repeat_worker` (`worker.rs:171`) — a polling loop that re-spawns `clud loop` once per `repeat_interval_secs`, never accepts attach connections.
2. Bind a non-blocking `TcpListener` on `127.0.0.1:0` for attaching clients; record `worker_port`.
3. Build the initial `SessionSnapshot` (`worker.rs:64`) and `WorkerShared` (`worker_shared.rs:50`); open the append-only log file (`init_log_file`, `worker_shared.rs:73`).
4. Start the backend: `start_subprocess_session` (`worker.rs:312`) or `start_pty_session` (`worker.rs:384`). Both spawn a stdout-drain thread that calls `shared.push_output(chunk)` and a wait-thread that joins the drain *before* calling `broadcast_exit` (`worker.rs:344`-`378`). That join is load-bearing: without it, `Exited` can race ahead of the final `Output` chunk on the per-client mpsc channel and an attached client silently loses the last line. macOS-ARM hit this most often in `test_attach_last` (PR #136).
5. Persist the snapshot, now with `root_pid` populated.
6. Spawn two background threads:
   - **Daemon-pid watchdog** (`worker.rs:114`-`132`): polls `pid_is_alive(daemon_pid)` every 200ms. If the daemon dies, the worker `runtime.cleanup_tree()`s the backend (Term, sleep 150ms, Kill across descendants), broadcasts exit 137, persists, removes the spec file, and exits.
   - **Heartbeat / dead-peer evictor** (`worker.rs:138`-`146`): every 2s calls `evict_dead_client` which zero-byte-peeks the attached TCP socket and releases the slot on `Ok(0)` / `ConnectionReset` / `ConnectionAborted`.
7. Accept loop runs `handle_worker_client` (`worker.rs:446`) per connection. Exits the outer loop when `stop_accepting` is set *and* no client is currently attached, then persists a final snapshot and removes the spec file.

## Attach flow

`clud attach <key>` walks through `run_attach` (`attach.rs:26`):

1. `ensure_daemon` — fast-paths if `daemon.json`'s listener answers a probe.
2. `resolve_session_id` (`sessions.rs:11`) tries exact id, then unique `name` match, then unique prefix match. Ambiguous matches list the candidates so the user can disambiguate.
3. `DaemonRequest::Session` fetches the current `SessionSnapshot` to read `worker_port`. Reject if `!session.attachable` (repeat jobs cannot be attached).
4. `attach_to_session` (`attach.rs:70`) opens a `TcpStream` to `worker_port`, writes `WorkerClientMessage::Attach`, reads the first reply. Three cases:
   - `Attached { session }` → enter the bidirectional bridge.
   - `Error "session already has an attached client"` → retry for up to 5s (`attach.rs:136`-`142`). Covers the brief window when an old client is still being evicted by the worker's heartbeat.
   - Any other reply (`Error`, premature `Output`, `Exited`) → print and return.
5. On the worker side, `handle_worker_client` (`worker.rs:446`) reads the `Attach` line, calls `attach_client` (`worker_shared.rs:195`) which takes the single-client slot, sets `background = false`, and returns the replay payload:
   - **PTY sessions**: exactly one chunk — `TerminalCapture::snapshot_bytes()` — a synthesized repaint of the current grid + cursor + alt-screen state. Raw backlog cannot be replayed mid-session because cursor moves and partial redraws stack into garbage when played from the middle of a session (issue #34). See `session-lifecycle.md` for the capture parser internals.
   - **Subprocess sessions**: the raw backlog — a deque of byte chunks capped at `DEFAULT_BACKLOG_LIMIT_BYTES` (256 KiB, overridable via `--backlog-size` or `CLUD_BACKLOG_BYTES`). Line-oriented output replays cleanly because each line is self-contained.
   - If `snapshot.exit_code` is already set the worker writes a final `Exited` and closes immediately.
6. The worker spawns a writer thread that drains its per-client mpsc receiver into the TCP stream, and the main connection thread enters a `read_worker_line` loop dispatching `Input` / `Resize` / `Interrupt` to the `SessionRuntime`.
7. On the client, a reader thread parses `Output` / `Exited` / `Error` and writes to stdout. In parallel, `run_remote_interactive` (`attach.rs:249`) puts the terminal in raw mode (`RawTerminalGuard`, `types.rs:212`), polls `crossterm` events, and runs each `KeyEvent` through `translate_key_event` (`keys.rs:5`):
   - `KeyAction::Forward(bytes)` → `WorkerClientMessage::Input { submit: bytes == b"\r" }`.
   - `Event::Paste(text)` → `Input { submit: false }`.
   - `Event::Resize(cols, rows)` → `WorkerClientMessage::Resize`.
   - `KeyAction::Interrupt` (Ctrl+C) → break the loop with `LocalAttachResult::InterruptRequested`.
8. On `InterruptRequested`: if `session.detachable`, show the 5s `BACKGROUND_PROMPT_TIMEOUT` prompt (`attach.rs:342`). Y/Enter/timeout → `shutdown_worker_connection` and return 0 (session continues in background). N/Esc → `request_session_termination` and return 130. If not detachable, send `WorkerClientMessage::Interrupt` to the worker and wait up to 5s for `Exited`.

## Snapshot and log persistence

API-managed logical conversations use a separate durable record family. See
[api-session-storage.md](api-session-storage.md): `api-sessions/<id>.json`
survives provider-turn completion as `idle` and must not be interpreted as a
worker `SessionSnapshot` or attach target.

Under `state_dir` (resolved by `paths.rs:7` — CLI flag > `CLUD_DAEMON_STATE_DIR` env > `temp_dir()/clud-daemon`):

```
daemon.json                    DaemonInfo { pid, port }       written once at daemon startup
daemon-events.jsonl            structured daemon event log     append-only, best-effort, rotated once at 1 MiB
sessions/<id>.json             SessionSnapshot                overwritten on every state change
specs/<id>.json                WorkerLaunchSpec               written by daemon, removed by worker on exit
logs/<id>.log                  append-only stdout/stderr      soft cap 10 MiB (LOG_ROTATE_BYTES)
logs/<id>.log.1                single rotation backup         overwritten on each rotation
<user transcript path>         optional output tee            via running_process::telemetry
```

All JSON files use `write_json_file` (`io_helpers.rs:33`): write to `.tmp`, `remove_file` existing, `rename` into place — atomic on POSIX and Windows. Snapshot writes happen on root-pid set, exit, repeat-state change, and `background` flip (`worker_shared.rs:155`-`193`). The log file is opened once in `init_log_file` and rotated when `metadata().len() >= LOG_ROTATE_BYTES` (`worker_shared.rs:93`-`127`); only one backup is kept because clud sessions are ephemeral and the on-disk footprint shouldn't grow unboundedly for a stale session nobody reattaches to.

`daemon-events.jsonl` is separate from backend output logs. Each line is one JSON object with `ts_ms`, `event_id`, `daemon_pid`, `thread`, `op`, and operation-specific fields. It records daemon lifecycle events, request/reply envelopes, GC dispatch, worker reap, `AdoptKill`, and orphan sweep start/finish records. The log is best-effort and non-fatal: write failures do not affect daemon behavior.

Issue #539 added a Windows `conhost.exe` reaper here (`daemon/conhost_reaper.rs`, a 60s sweep emitting `conhost_reap_finished`). It was removed in #612: its orphan test was a conhost's **parent** PID being dead, but a conhost's parent is whoever *created* the console, while the processes that depend on it are its clients — so the predicate carried no liveness information. In practice it could only match healthy consoles a few hundred milliseconds old (git-bash's `bin\bash.exe` → `usr\bin\bash.exe` re-exec makes a transiently-dead parent routine), and killing a conhost destroys the console, taking the attached session down. Windows already terminates a client-less conhost on its own within ~0.5s, and `running-process-core` 3.4+ places every `NativeProcess` in a kill-on-close Job Object that contains the conhosts its children create.

Every `push_output` chunk goes to three sinks (`worker_shared.rs:299`-`331`): the in-memory backlog (with eviction at the byte cap), the `TerminalCapture` parser if active, and the log file. The Output mpsc to the attached client is fed last, after all persistence side-effects, so a crash mid-`push_output` cannot leave the client ahead of the on-disk state.

## Key types

- `SessionSnapshot` — on-disk + wire session metadata — `types.rs:48`
- `WorkerLaunchSpec` — daemon → worker launch contract (boxed inside `DaemonRequest::Create`) — `types.rs:77`
- `DaemonRequest` / `DaemonResponse` — `types.rs:103`, `types.rs:111`
- `WorkerClientMessage` / `WorkerServerMessage` — `types.rs:120`, `types.rs:129`
- `SessionRuntime` — runtime abstraction over subprocess and PTY — `types.rs:137`
- `SessionKind` (`Subprocess` / `Pty`) — `types.rs:36`
- `WorkerShared` — per-worker shared state, owns the single-client gate — `worker_shared.rs:22`
- `AttachedClient` — single-client slot (id, mpsc sender, shutdown handle, attach instant) — `types.rs:234`
- `BacklogState` — bounded chunk deque + total byte count — `types.rs:242`
- `RawTerminalGuard` — RAII raw-mode guard — `types.rs:212`
- `LocalAttachResult` / `BackgroundPromptDecision` — attach-flow control enums — `types.rs:201`, `types.rs:207`
- `ENV_FEATURE_FLAG = "CLUD_EXPERIMENTAL_DAEMON"` — `types.rs:17`
- `ENV_STATE_DIR = "CLUD_DAEMON_STATE_DIR"` — `types.rs:18`
- `ENV_BACKLOG_BYTES = "CLUD_BACKLOG_BYTES"` — `types.rs:19`
- `DEFAULT_BACKLOG_LIMIT_BYTES = 256 KiB` — `types.rs:20`
- `LOG_ROTATE_BYTES = 10 MiB` — `types.rs:28`
- `BACKGROUND_PROMPT_TIMEOUT = 5s` — `types.rs:21`

## Failure modes

**Daemon dead, client wants to start a session.** `ensure_daemon` (`client.rs:43`) probes the port from `daemon.json`; on failure it runs `cleanup_stale_state` to drop the stale `daemon.json` and spawns a fresh `__daemon`. The client retries up to 5s for the new listener to come up; longer than 5s returns `io::ErrorKind::TimedOut`.

**Daemon dead, worker still running.** `run_worker`'s watchdog thread polls `pid_is_alive(daemon_pid)` every 200ms (`worker.rs:114`-`132`). On daemon death it calls `runtime.cleanup_tree()` (Term, 150ms sleep, Kill across every descendant via `signal_process_tree`), broadcasts exit 137, persists the snapshot, removes the spec file, and the worker process exits. The orphaned session shows up in the next `clud list` only after a daemon restart triggers `cleanup_stale_state` — but the snapshot already has `exit_code = Some(137)`.

**Worker crashes mid-session (e.g. SIGSEGV).** The daemon's `reap_worker_when_done` removes the entry from the workers map but does NOT touch `sessions/<id>.json`. The stale snapshot is reaped on the next `cleanup_stale_state` call (next `ensure_daemon`): `pid_is_alive(snapshot.worker_pid)` is false, the snapshot is flipped to `exit_code = Some(137)` and `background = false`. Subsequent `clud list` / `clud attach` see an exited session and a `clud logs` post-mortem still works because the log file was append-only and survives.

**Snapshot file unwritable.** Worker startup aborts (`worker.rs:109`-`112`) and the daemon's create-response readiness loop (`server.rs:152`-`196`) times out at 5s — either no snapshot ever appeared, or it appeared but the worker port never accepts connections. Daemon replies `error "worker wrote snapshot but TCP port N is not accepting connections"`.

**Attach to non-existent session.** `resolve_session_id` returns `session 'X' not found`. Same path for an ambiguous prefix/name; the error lists the candidate ids. `clud logs` additionally falls back to checking for a bare `logs/<X>.log` file (`commands.rs:175`-`182`) so post-mortem log access works even after the snapshot has been GC'd.

**Two clients race to attach.** The second one gets `error "session already has an attached client"`. The client side retries that specific message for up to 5s (`attach.rs:136`-`142`) so the common case of "old client just disconnected" succeeds without manual retry. After 5s the client gives up and returns 1.

**Attached client's terminal crashes (SSH drop, terminal app killed, etc.).** The worker's 2s heartbeat probes the TCP peer with a zero-byte `peek`; `Ok(0)` / `ConnectionReset` / `ConnectionAborted` trigger `evict_dead_client` (`worker_shared.rs:264`) which releases the single-client slot and flips `background` back to true. A new `clud attach` succeeds immediately and gets a fresh capture snapshot.

**Spec file orphaned.** If a worker dies before writing its snapshot, `cleanup_stale_state` removes spec files older than 10s with no matching snapshot. The 10s grace covers normal worker startup latency.

**`Output` after `Exited` race (historical).** Subprocess and PTY backends both spawn a drain thread that calls `push_output` and a separate wait thread that calls `broadcast_exit`. Before PR #136, the wait thread could enqueue `Exited` on the client mpsc before the drain's final `Output` chunk had landed, silently dropping the backend's last line. The fix joins the drain handle inside the wait thread before `broadcast_exit` (`worker.rs:373`-`378` and `worker.rs:436`-`440`). Keep this invariant when modifying either start function.

**Repeat-job worker tries to accept attach.** `attachable: false` is set in the spec for repeat jobs (`entry.rs:204`); `run_attach` rejects with `session ... is a repeat job and cannot be attached`; `list_attachable_sessions` filters them out of the auto-attach single-session shortcut.

## See also

- `../../crates/clud-bin/src/daemon/README.md` — per-file directory README with the full `file:line` index of public items.
- `session-lifecycle.md` — `TerminalCapture` parser, PTY pump, input injection, the exact bytes the repaint payload contains.
- `gc-and-registry.md` — the GC half of the same daemon process: `~/.clud/data.redb` is owned by an in-process registry-worker thread and served via the `DaemonRequest::Gc { payload }` variant on this same TCP listener (issue #135 / DD-012).
- `launch-plan.md` — `LaunchPlan` is the inner payload that `WorkerLaunchSpec` wraps and ships to the worker.
- `../DESIGN_DECISIONS.md` — rationale for single-binary re-entry over a separate daemon executable, and atomic file writes over a real database. The original "TCP+JSON over named pipes / Unix sockets" decision has been **reversed**: the broker frame lane is a named pipe on Windows and a Unix socket elsewhere, and it is tried first. See DD-025.
