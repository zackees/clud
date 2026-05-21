# mock-agent/

A tiny Rust binary that masquerades as `claude` or `codex` during
integration tests. Tests copy it onto a temp `PATH` so `clud` resolves a
real, controllable executable instead of the actual agent. See
[`src/README.md`](src/README.md) for the behavior contract (argv echo,
stdin capture, PTY size reporting, exit-code controls).

## Layout

- `Cargo.toml` — workspace member, `publish = false`, depends on
  `serde_json`, `terminal_size`, and `libc` (unix only).
- [`src/`](src/README.md) — single-file binary (`src/main.rs`) plus its
  behavior contract.

## Build

The crate is a workspace member of the root `Cargo.toml`, so it builds
alongside everything else. To build just this binary:

```
soldr cargo build -p mock-agent
```

Output lands at `target/debug/mock-agent` (`.exe` on Windows). All
`cargo` invocations must go through `soldr` per the repo policy in
`CLAUDE.md`.

## Used by

- **Python integration tests** (`tests/integration/`) — `conftest.py`
  builds it once per session via the `mock_agent_binary` fixture, then
  the `mock_env` / `mock_env_codex_cmd` fixtures copy it as
  `claude{.exe}` and `codex{.exe}` onto a temp `PATH`. Consumed by
  `test_mock_agents.py`, `test_loop_stream_json.py`,
  `test_voice_mode.py`, `test_daemon_persistence.py`,
  `test_daemon_cleanup.py`, and `test_session_registry_concurrency.py`.
- **Rust PTY integration tests** (`crates/clud-bin/tests/pty_behavior.rs`)
  — `mock_agent_path()` locates the freshest `target/.../mock-agent`
  and falls back to `cargo build -p mock-agent --message-format json`
  if the binary is missing. `ci/test.py` pre-builds it so this
  fallback rarely runs.
