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
