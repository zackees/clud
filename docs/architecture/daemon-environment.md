# Daemon session environment

A long-lived daemon must not make the first shell that auto-started it the
permanent environment for every later session. This document owns the
admission-time environment layering for daemon-hosted workers (#933).

## Layers

Each worker gets one immutable environment assembled at daemon admission:

```text
materialized OS/login environment
  └── daemon-private values (`CLUD_DAEMON_*`, `RUNNING_PROCESS_*`)
        └── initiating client's environment
              └── child policy (`IN_CLUD`, temp dir, nounset, rm shim, ...)
```

The client overlay wins key-for-key. In particular, `PATH` is replaced rather
than unioned: retaining the daemon's stale prefix would still select a shadowed
old binary. The ordinary foreground runner starts from the initiating process
and applies the same child-policy function, so both paths agree once the client
layer is present.

`WorkerLaunchSpec.login_env` persists the first two layers alongside the client
environment. A worker therefore never consults its inherited environment to
construct the backend's environment, and a refresh cannot change an already
running session.

## Materializing the login layer

The daemon takes an initial snapshot at startup, refreshes before every new
session, and runs a five-minute idle backstop. Failed refreshes retain the last
known-good snapshot.

- **POSIX:** clud launches the account's login shell with a minimal cleared
  bootstrap environment and captures its fixed `env -0` output. Profile
  evaluation is bounded to three seconds; a timeout or non-zero exit preserves
  the prior snapshot. The static `env -0` command accepts no caller input, and
  the clear environment prevents the initiating shell's activated venv,
  toolchain shims, or credentials becoming the login floor.
- **Windows:** clud reads the machine environment registry key, overlays the
  user key, expands `REG_EXPAND_SZ` values, and composes `PATH` as machine then
  user. `SystemRoot`, `SystemDrive`, and `USERPROFILE` retain their
  loader-provided values if the persisted keys do not name them, so ordinary
  `%SystemRoot%` entries remain usable.

Session-local values are deliberately absent from this base. They are freshest
and most predictable only when received in `WorkerLaunchSpec.client_env`.

## Compatibility and scope

Both environment fields use `#[serde(default)]`. A worker spec emitted before
this feature has no login snapshot and falls back to the daemon process
environment, preserving rolling-upgrade behavior rather than creating an empty
child environment.

API-session turns do not yet persist an initiating-client environment; they
continue through the existing `child_env_from` compatibility path. Extending
that durable API record is separate work, because an HTTP request's environment
cannot be recovered after that request returns.

## Source owners

- `daemon/login_env.rs` — platform materialization, refresh state, and parser.
- `daemon/server.rs` — starts periodic refresh and snapshots the base at
  `Create` admission.
- `daemon/types.rs` — durable `WorkerLaunchSpec.login_env` wire field.
- `daemon/io_helpers.rs` — login/client merge plus shared policy application.
- `daemon/worker.rs` — applies the persisted base for PTY, subprocess, bridge,
  and repeat worker paths.
