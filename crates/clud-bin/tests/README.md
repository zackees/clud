# tests/

Rust integration tests for the `clud-bin` crate. Unlike the `#[test]` units inside `src/`, these tests spawn the workspace `mock-agent` binary through `running_process_core::pty::NativePtyProcess` and exercise the real PTY pump used by `clud --codex`. They lock down platform-specific contracts (Windows ConPTY vs POSIX), cross-platform regressions from issues #28/#31/#46, and the voice/F3 + resize hooks implemented in `clud::session`. A `pty_canary()` probe runs first and skips the test if the host's stdout isn't a real console (typical in nested shells or captured `cargo test` runs).

## Files

- `pty_behavior.rs` — End-to-end PTY checks: `respond_to_queries_impl` DSR stub (T1), `resize_impl` propagate-on-POSIX / no-op-on-Windows (T2), extreme `cols=32767` spawn safety (T3), verbatim stdin forwarding through `run_raw_pty_pump`, F3 press/release detection (xterm + kitty CSI-u), idle `on_tick` cadence, Ctrl-C flag honoring, resize channel application, prompt exit on child death, and raw-mode recovery on hook panic.

## How to run

From the repo root:

```bash
bash test                                   # Rust + Python unit tests
bash test --integration                     # adds mock-agent integration tests
soldr cargo test -p clud-bin                # all clud-bin tests (unit + integration)
soldr cargo test -p clud-bin --test pty_behavior   # this file only
soldr cargo test -p clud-bin --test pty_behavior -- --nocapture   # see canary-skip diagnostics
```

All `cargo`/`rustc`/`rustfmt` invocations must go through `soldr` (see root `CLAUDE.md`). The mock-agent is auto-built on first run via `cargo build -p mock-agent --message-format json`.
