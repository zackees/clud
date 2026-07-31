# tests/

Rust integration tests for the `clud-bin` crate. Unlike the `#[test]` units inside `src/`, these tests spawn the workspace `mock-agent` binary through `running_process_core::pty::NativePtyProcess` and exercise the real PTY pump used by `clud --codex`. They lock down platform-specific contracts (Windows ConPTY vs POSIX), cross-platform regressions from issues #28/#31/#46, and the voice/F3 + resize hooks implemented in `clud::session`. A `pty_canary()` probe runs first and skips the test if the host's stdout isn't a real console (typical in nested shells or captured `cargo test` runs).

## Files

- `pty_behavior.rs` — End-to-end PTY checks: `respond_to_queries_impl` DSR stub (T1), `resize_impl` propagate-on-POSIX / no-op-on-Windows (T2), extreme `cols=32767` spawn safety (T3), verbatim stdin forwarding through `run_raw_pty_pump`, F3 press/release detection (xterm + kitty CSI-u), idle `on_tick` cadence, Ctrl-C flag honoring, resize channel application, prompt exit on child death, and raw-mode recovery on hook panic.
- `pty_pump.rs` — Raw PTY pump contracts: verbatim stdin forwarding (#46), voice F3 press/release hooks (#13/#41), idle ticks, Ctrl-C/interrupt propagation (flag + extra_rx 0x03), resize-channel delivery, prompt exit on child death, hook-panic recovery, Shift+Enter extra_rx round trip (#141), and the issue #538 writer-thread decoupling (`stdin_forwarding_stays_fast_while_output_sink_stalls`, via the `run_raw_pty_pump_full_with_writer_for_test` sink-injection seam).
- `shift_enter_dual_reader.rs` — Windows native terminal-input consumer regression: sends native key records through running-process's translator, verifies complete arrow/Home/End/Insert/Delete/Page sequences and trace bytes (#575), and exercises the real `WriteConsoleInputW` → `TerminalInputCore` → clud policy-adapter path when a console is attached; also preserves Shift+Enter's literal-LF contract (#141).
- `win32_hooking_probe.rs` — Ignored Windows-only #468 research probe for raw Job Object lifecycle events, PEB command-line reads, handle snapshots, breakaway denial, and LoadLibrary DLL injection against `testbins/probe-*`.
- `tool_shell_lifecycle_windows.rs` — Windows-only #616 live Job completion-port coverage: a registered fake agent's direct leaked client is reaped while an intentionally detached nested `cmd.exe` survives until explicit cleanup, and (#673 Phase 5) the session's reap summary and `reap.jsonl` name the kill.
- `reaper_daemon_survival_windows.rs` — Windows-only #674 daemon-survival coverage against a real Job Object: all three real detach shapes survive a tool-shell lifecycle, a genuinely leaked client is still reaped, and one session's exit does not reach another session's daemon. Uses `testbins/daemon-stub`, never a real sccache/docker install.
- `reaper_batch_drain_windows.rs` — Windows-only #706 completion-port drain coverage: a 48-process burst queues notifications faster than the listener consumes them, proving the drain folds them into batches that share **one** host process-table enumeration (measured 5 scans, not ~48). The folding rule itself is Tier 1 (`job_orphan_reaper::batch_tests`); only the real port's queuing behaviour needs a real Job Object. Prints its measurement under `--nocapture`.
- `wedge_watchdog_e2e.rs` — Ignored Windows-only #541 end-to-end probe for the wedge watchdog (`clud::wedge_watchdog`). Drives the real Toolhelp32/`GetThreadTimes`/`GetProcessIoCounters` sampler through the public `WedgeWatchdog` API against real spinning threads in the test process itself (no separate testbin needed — the sampler walks the subtree rooted at whatever pid it's given). Covers all three acceptance-criterion shapes: quiet single-thread spin reaches `Wedged`, spin-with-concurrent-IO stays `Healthy`, and spread multi-thread load stays `Healthy`.

## Where a reaper test goes

**Prefer unit tests. They are faster and they run on every platform.** An
integration test is justified only when the behaviour cannot be expressed
against injected process facts — in practice, only when a real Job Object or
real process detachment is the thing under test. Reap decisions take OS facts
as data (`ProcessFacts` / `FactsSnapshot`), so the decision table belongs in
`job_orphan_reaper`'s `lifecycle_tests` as a Tier 1 unit case asserting **spare
+ reason**, not just the outcome.

Integration coverage here has a stated budget of **≤5 tests**; anything
expressible in Tier 1 must be in Tier 1. Full tier rules, the daemon signal
table and the cooperative-marker caveat:
[`docs/architecture/process-reaping.md`](../../../docs/architecture/process-reaping.md).

## How to run

From the repo root:

```bash
bash test                                   # Rust + Python unit tests
bash test --integration                     # adds mock-agent integration tests
soldr cargo test -p clud-bin                # all clud-bin tests (unit + integration)
soldr cargo test -p clud-bin --test pty_behavior   # this file only
soldr cargo test -p clud-bin --test pty_behavior -- --nocapture   # see canary-skip diagnostics
soldr cargo test -p clud --test win32_hooking_probe -- --ignored --nocapture --test-threads=1
soldr cargo test -p clud --test wedge_watchdog_e2e -- --ignored --nocapture --test-threads=1
```

All `cargo`/`rustc`/`rustfmt` invocations must go through `soldr` (see root `CLAUDE.md`). The mock-agent is auto-built on first run via `cargo build -p mock-agent --message-format json`.
