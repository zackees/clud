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

## Memory subsystem tests

The memory subsystem's per-module unit tests live next to each `.rs` file under `crates/clud-bin/src/memory/` and `crates/clud-bin/src/daemon/{memory_service.rs, memory_mcp.rs, http.rs}`. Their `#[test]` blocks (search for `mod tests { ... }`) cover store inserts, hybrid search RRF math, tier transitions, the HTTP route bodies, hook handlers, MCP tools, and git-artifact roundtrips.

Cross-cutting tests that span modules — and therefore can't live in any one source file — sit on the Python side under [`tests/`](../../../tests/):

- [`tests/test_memory_cross_process.py`](../../../tests/test_memory_cross_process.py) — save in one daemon, kill it, recall in the next. Canonical RED test from META #255 (issue #266).
- [`tests/test_memory_perf_budget.py`](../../../tests/test_memory_perf_budget.py) — 30 ms / 25 ms / 60 MB / 15 MB-per-1k budgets. Smoke variants always run; heavy iterations gated on `pytest -m perf_budget`.
- [`tests/integration/test_memory_e2e.py`](../../../tests/integration/test_memory_e2e.py) — full Claude / Codex session lifecycle through the four hook subcommands.

Hook payload fixtures live under [`testbins/mock-hooks-payloads/fixtures/`](../../../testbins/mock-hooks-payloads/README.md) — JSON files named `<hook_verb>_<variant>.json` covering Claude + Codex shapes.
