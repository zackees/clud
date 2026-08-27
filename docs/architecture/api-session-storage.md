# API Session Storage

Issue #1041 adds the durable storage foundation for API-managed agent conversations. It deliberately sits above existing daemon worker snapshots and does not launch a provider process or expose an HTTP route.

## Two session lifetimes

`sessions/<id>.json` is a `SessionSnapshot` for one daemon worker. Its PID, attach port, and exit code describe that worker's lifetime. `api-sessions/<id>.json` is an `ApiSessionRecord` for a logical provider conversation. It owns a stable clud ID, backend, provider session/thread ID when captured, canonical CWD, resolved settings, turn history, and state. A successful turn leaves the record `idle`, so a later controller can explicitly resume the same provider conversation.

The two record families must not be merged. API records have no attach port and are not discovered by worker-snapshot listers.

## Persistence and retention

Creation canonicalizes the requested CWD; it must exist and be absolute. That canonical value is persisted once and no future turn can override it. The resolved settings projection stores model, safe mode, model provider, harness, and routing mode without accepting raw argv or environment data.

Writes use the daemon JSON atomic-write helper. Mutations take an `fs4` exclusive lock at `api-sessions/<id>.lock`, reload, apply one transition, then persist; creation has its own `create.lock`. Generated logical IDs remain distinct from provider IDs. New fields are serde-defaulted for older records.

Events receive monotonically increasing cursors; only the newest 512 are retained, and one event payload may not exceed 32 KiB. The idempotency ledger retains 128 request fingerprints: the same key/fingerprint returns the original mapping while conflicting reuse fails. This store is bounded metadata, not dashboard state, telemetry, or an unbounded transcript.

## Restart and corruption

After daemon restart, persisted PIDs are never trusted as ownership. `starting`, `running`, and `interrupting` records become `failed`; their active turns seal with `daemon_restart`, while a captured provider ID remains available for a later explicit resume. The store never probes, signals, or kills a stale identity.

Unreadable records return a corrupt-state error, are skipped by ordinary listing, and can be moved aside with `quarantine_corrupt`; evidence is renamed, never overwritten. Later lifecycle and HTTP slices own user-facing recovery and execution.

## Captured turn execution

The #1042 controller accepts a canonical headless `LaunchPlan` only when its
backend and CWD exactly match the durable record. It uses a captured,
subprocess-only `NativeProcess` and the invisible daemon-helper flags on
Windows. A dedicated drain appends raw JSONL to
`logs/api/<session-id>/<generation>.jsonl`, emits bounded raw/normalized
events, and persists a recognized provider ID. The waiter joins this drain
before sealing the turn, so the last identity/output event cannot race normal
completion into `idle`. Unknown provider records are opaque events and
malformed records become diagnostics.

## HTTP contract

`/v1/*` is loopback, Host-validated, and bearer-only; dashboard query-token
and cookie bootstrap never authorize it. Typed session routes create/list/get
durable records, expose bounded `after` cursor events with an explicit
`limit` from 1 through 128, and map invalid input,
missing IDs, and active conflicts to stable JSON errors. No route accepts raw
argv, environment injection, or a per-turn CWD override.
The event response is `{events,next_cursor,retention_gap}`; a retention gap
explicitly tells a poller that its cursor predates the bounded retained window.

CLI `clud kill <logical-id>` uses the additive daemon `api_session_kill` RPC.
It is distinct from `terminate`, which remains exclusively the worker
`SessionSnapshot` termination contract. `clud list` and `clud logs` continue
to read their respective local bounded stores; no API session is projected as
an attachable worker snapshot.

`POST /v1/sessions/{id}/turns` accepts `{ "message": string,
"interrupt_running": boolean }` and an optional `Idempotency-Key` header.
It reconstructs a subprocess-only `LaunchPlan` from the record's persisted
settings and immutable canonical CWD. Generation zero creates a provider
conversation; later generations require the captured provider identity. The
only successful responses are `202 started` and `200 replayed`; active,
terminated, missing-identity, and conflicting retry cases are stable JSON
errors (`409`). No request can supply a CWD, raw argv, or environment.

The existing `clud list`, `clud logs <logical-id>`, and `clud kill
<logical-id>` commands recognize logical API IDs separately from worker
snapshots. Dashboard rows have `source: "api"`, `kind: "api"`, and
`attachable: false`; an API session is never eligible for the attach transport.
The daemon shares one API lifecycle manager across HTTP and both daemon RPC
transports, so an IPC kill reaches the captured child rather than only its
durable record.
Bearer capabilities are returned only by `clud daemon api-info --json`, never
by dashboard state, logs, error payloads, or OpenAPI output.

Captured API children do not inherit the daemon dashboard/API capability
environment, so those capabilities cannot be reflected into raw turn output.
Provider JSONL itself is retained verbatim in the per-turn raw log and bounded
event stream for diagnostics; it is sensitive provider output, not a generic
secret-redaction service. API callers must therefore treat events and logs as
sensitive data and avoid emitting credentials through their provider tooling.

## Lifecycle serialization

The #1043 lifecycle controller keeps one in-memory captured-process handle per
logical ID, with a per-logical-ID mutation gate around interrupt/replace/start
sequencing. Its durable admission transition takes the per-session file lock,
checks the idempotency ledger, and records both a new generation and its key in
one write *before* a subprocess is created. A competing normal submission
receives `session_busy`; a duplicate idempotency key with the same fingerprint
replays its original turn ID even after a controller restart, while a different
fingerprint conflicts. A later resume requires an already-captured provider
identity. Replacement removes the active handle, durably enters
`interrupting`, requests an interrupt, waits a bounded grace period, records
graceful or forced disposition, then starts the next generation. Terminal kill
signals the captured tree, seals the current turn as killed, and makes the
durable session `terminated`, so future submissions are refused. These
controls use only API captured subprocesses and never alter legacy interactive
attach Ctrl-C behavior.
