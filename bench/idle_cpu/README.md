# Idle CPU benchmark

`python -m bench.idle_cpu.harness` starts a fresh daemon and idle mock-agent
sessions, then records cumulative per-process CPU time and daemon-event writes
over a fixed window. It is a local/scheduled measurement tool, not a default CI
test: absolute CPU time varies with host load.

## Run

From the repository root, first build the same binaries used by the integration
suite (the harness will also do this through `soldr` when they are missing):

```bash
bash test --integration -- tests/integration/test_daemon_restart.py
python -m bench.idle_cpu.harness --sessions 1 --window-secs 60 --json bench/idle_cpu/baseline_n1.json
python -m bench.idle_cpu.harness --sessions 8 --window-secs 60 --json bench/idle_cpu/baseline_n8.json
```

`CLUD_TEST_BINARY` and `CLUD_TEST_MOCK_AGENT_BINARY` can point at already-built
test binaries. The default `--mode daemon` launches `--detach --codex` sessions
without a PTY, so it measures daemon-managed idle work rather than terminal
rendering. `--mode pty` measures the foreground PTY pump instead: each session
is `clud --pty` inside a pseudo-terminal, as in an interactive launch (#691).
Its baselines are `baseline_pty_n1.json` and `baseline_pty_n6.json`:

```bash
python -m bench.idle_cpu.harness --mode pty --sessions 1 --window-secs 60 --json bench/idle_cpu/baseline_pty_n1.json
python -m bench.idle_cpu.harness --mode pty --sessions 6 --window-secs 60 --json bench/idle_cpu/baseline_pty_n6.json
```

Every launch runs with a throwaway HOME and working directory. A dev build of
clud rewrites per-user state (including the `~/.clud/state/rm-shim/rm` shim
the installed `clud-cmd-scan` hook checks) and the launch directory's
`.claude/settings.json` / `.codex/hooks.json`, so it must never see the real
ones.

## Read and enforce

The JSON report contains a `per_process` list (role, PID, CPU-seconds and
best-effort context-switch delta) plus `totals.client_cpu_seconds`,
`totals.daemon_cpu_seconds`, and `totals.event_lines_appended`. The event count
includes both active and rotated `daemon-events.jsonl` files.

Use opt-in budget mode locally or in a scheduled job:

```bash
python -m bench.idle_cpu.harness --sessions 1 --window-secs 60 --budget
CLUD_BENCH_BUDGET=1 python -m bench.idle_cpu.harness --sessions 8 --window-secs 60
```

CPU budgets allow 20% over the selected baseline; the event budget allows one
line. To prove the no-op `gc.insert` regression signal, copy a baseline, set
`totals.event_lines_appended` to `0`, then run with `--budget --baseline <copy>`:
the current no-op event stream must fail. Once #543 and #544 land, refresh the
committed baselines so the normal budget makes that regression fail directly.

## Update baselines

Run the two 60-second commands above on a quiet representative machine after an
intentional idle-cost change. Commit the resulting JSON together with the PR and
state the machine/OS and before/after totals in the PR body. Never update a
baseline merely to hide an unexplained regression.
