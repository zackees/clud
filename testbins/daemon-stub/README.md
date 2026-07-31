# daemon-stub

Long-lived daemon stand-in for the reaper survival suite ([#674]).

A clud session must never kill a daemon started inside it. The daemons that
matter — zccache, soldr, sccache, `FBuildWorker`, dockerd, language servers —
detach in **different ways**, and clud's spare-list keys on the OS signals that
result, not on image names. This binary reproduces each of those signal shapes.

Depending on a real sccache or Docker being installed would make the suite
non-hermetic and unrunnable on a clean CI runner. Reproducing the signal shape
is what the code actually keys on, so that is what the stub does.

## Modes

```
daemon-stub <mode> <pid-file-path>
```

| Mode | Signal shape | Real-world analogue | Protected by |
|---|---|---|---|
| `serve` | binds `127.0.0.1:0`, writes its PID, stays up | (the daemon itself) | — |
| `spawn-breakaway` | outside the caller's Job Object, daemon marker set | anything using `spawn_daemon_breaking_away_from_job` | job-object membership |
| `spawn-marked` | **inside** the Job Object, daemon marker set | zccache, soldr | the cooperative marker |
| `spawn-detached` | inside the Job Object, **no** marker, own detach, owns a listening socket | **sccache**, `FBuildWorker`, language servers | listening-endpoint ownership |

`spawn-detached` is the hard case and the reason this crate exists: those
daemons never call `running-process`, so the cooperative
`RUNNING_PROCESS_IS_DAEMON` marker gives them nothing. It explicitly strips the
marker from the child's environment so the case can never pass by accident.

Every spawn mode writes the served child's PID to the given path and exits, so
a test can assert on that PID's survival without owning the child. The served
process exits on its own after two minutes, so a crashed test does not leave it
running.

## Why raw `std::process::Command`

`testbins/` is outside `ci/banned_imports.py`'s scan, and `spawn-detached` must
detach *without* `running-process`: a `NativeProcess` would attach its own
containment and set the very marker whose absence is under test.

## Consumers

- `crates/clud-bin/tests/reaper_daemon_survival_windows.rs` — Tier 2 of #674.
  Four tests, deliberately; anything expressible against injected `ProcessFacts`
  belongs in Tier 1 instead.
- `bench/README.md` — the Tier 3 opt-in runbook for checking the stub's signal
  shapes against genuinely installed daemons.

[#674]: https://github.com/zackees/clud/issues/674
