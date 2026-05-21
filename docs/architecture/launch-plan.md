# Launch Plan

Every code path that decides "what would clud actually run" funnels through
`command::build_launch_plan` and consumes the resulting `LaunchPlan`. There is
no other place in the binary where the backend argv, iteration budget, working
directory, repeat schedule, DONE/BLOCKED marker paths, or stream-json injection
get reconstructed — the runners, the daemon worker, the `--dry-run` JSON
emitter, the hook-health remediator, and the durable loop artifacts all read
the same struct. If you are changing the argv clud sends to `claude` or
`codex`, or adding a new subcommand, this is the only place in the tree that
needs to learn about it.

## The struct

`LaunchPlan` lives in `crates/clud-bin/src/command/types.rs:6`. Trimmed shape:

```rust
pub struct LaunchPlan {
    pub command: Vec<String>,           // argv: command[0] is the backend exe
    pub iterations: u32,                // 1 for one-shot; >1 for `clud loop`
    pub backend: Backend,               // Claude | Codex
    pub launch_mode: LaunchMode,        // Subprocess | Pty
    pub cwd: Option<String>,            // snapshot of std::env::current_dir()
    pub repeat_schedule: Option<RepeatSchedule>, // Some(interval_secs) iff --repeat
    pub task_summary: Option<String>,   // short label for session name
    pub loop_markers: Option<LoopMarkers>,       // DONE/BLOCKED absolute paths
    pub stream_json_progress: bool,     // claude subprocess-mode loop only
}
```

`LoopMarkers { done_path, blocked_path }` is at `types.rs:28`,
`RepeatSchedule { interval_secs }` at `types.rs:34`. All three derive
`Serialize` / `Deserialize` so the plan round-trips through the daemon's
`WorkerLaunchSpec` (`crates/clud-bin/src/daemon/types.rs:78`) and through
`--dry-run` JSON.

## Construction pipeline

`build_launch_plan(args, backend, backend_path) -> LaunchPlan` is at
`crates/clud-bin/src/command/builder.rs:24`. In order, it:

1. **Seeds `cmd` with `backend_path`** (`builder.rs:25`).
2. **Resolves codex sub-keyword.** Codex needs `exec` for non-interactive
   prompts and `resume` for `-c` / `--resume` continuations. The
   `has_noninteractive_prompt` predicate (`builder.rs:13`) decides which.
3. **Injects YOLO** unless `args.safe` — `--dangerously-skip-permissions` for
   Claude, `--dangerously-bypass-approvals-and-sandbox` for Codex
   (`builder.rs:41`).
4. **Threads `--model` / `-m`** with the backend-specific spelling
   (`builder.rs:48`).
5. **Synthesizes the prompt** per subcommand:
   - `Loop` resolves the positional via
     `command::loop_task::resolve_loop_task` (GH URL → `gh` fetch + cache,
     `#42` shortform, file path, or literal — see
     `crates/clud-bin/src/command/loop_task.rs:29`), then optionally appends
     the `done_marker_contract(...)` so the model writes to the exact path
     clud is polling (`builder.rs:98`, issue #95).
   - `Up`, `Rebase`, `Fix` route through the static templates in
     `crates/clud-bin/src/command/prompts.rs` and the backend-aware
     `push_prompt` helper (`prompts.rs:74`): Claude gets `-p <prompt>`, Codex
     gets the prompt as a positional argument.
6. **Materializes `loop_markers`** when DONE/BLOCKED polling is on
   (`builder.rs:112`), via `resolve_marker_paths` (`loop_task.rs:8`).
7. **Parses `--repeat`** with `parse_repeat_interval` (`builder.rs:250`):
   `30s` / `5m` / `1h` only, integer-positive, no compound or fractional
   forms.
8. **Forwards unknown flags** from `args.passthrough` (`builder.rs:176`).
9. **Resolves launch mode** via `backend::resolve_launch_mode`, taking
   `--pty`/`--subprocess`, the backend, the codex-exec bit, the
   is-loop bit, and parent-TTY detection into account (`builder.rs:181`).
10. **Injects stream-json progress flags** for Claude subprocess-mode loops
    (`builder.rs:200`): `--output-format stream-json --verbose` is spliced
    in immediately before the `-p` so the prompt stays at `command[-1]`.

## Consumers

Every code path that runs (or describes) the resolved argv reads from a
`LaunchPlan`:

- `crates/clud-bin/src/main.rs:222` — `build_launch_plan` is called once.
- `crates/clud-bin/src/main.rs:233` — `--dry-run` JSON emission (see contract
  below); exits 0 without spawning.
- `crates/clud-bin/src/runner.rs:110` (`run_plan_subprocess`) and
  `crates/clud-bin/src/runner.rs:416` (`run_plan_pty`) — per-iteration child
  spawn, reading `plan.command`, `plan.cwd`, `plan.iterations`, and
  `plan.stream_json_progress`.
- `crates/clud-bin/src/daemon/entry.rs:144` — `run_centralized_session` clones
  the plan into a `WorkerLaunchSpec` and ships it over IPC.
- `crates/clud-bin/src/daemon/worker.rs:67` and `:317` — worker process
  re-spawns the backend using `spec.plan.command` and `spec.plan.cwd`.
- `crates/clud-bin/src/hook_health.rs:800` — `run_backend_prompt` synthesizes
  a private `Args`, calls `build_launch_plan`, and runs the resulting argv
  as a one-shot subprocess for hook-migration prompting (`--fix-hooks`).
- `crates/clud-bin/src/loop_artifacts.rs:184` — `LoopSession::start` consumes
  `plan.iterations` to seed `TaskInfo::total_iterations` written to
  `<git-root>/.clud/loop/info.json`.

## Backend-specific divergence

| Concern | Claude | Codex |
|---|---|---|
| Subcommand keyword | (none) | `exec` for non-interactive prompt; `resume` for `-c`/`--resume` (`builder.rs:35`) |
| YOLO flag | `--dangerously-skip-permissions` | `--dangerously-bypass-approvals-and-sandbox` (`builder.rs:41`) |
| Model flag | `--model <id>` | `-m <id>` (`builder.rs:48`) |
| Prompt delivery | `-p <prompt>` | bare positional (`prompts.rs:74`) |
| `-m <message>` | `-m <message>` passthrough | dropped (would clobber `--model`; `builder.rs:148`) |
| `--continue` | `--continue` | `resume --last` (`builder.rs:62`) |
| `--resume <id>` | `--resume <id>` | `resume <id>` positional (`builder.rs:156`) |
| Stream-json progress | `--output-format stream-json --verbose` injected before `-p` for subprocess-mode loops (`builder.rs:200`) | not exposed by codex; skipped |

## YOLO injection

YOLO is on by default. The `--safe` flag is the opt-out — when set,
`build_launch_plan` skips the YOLO push entirely (`builder.rs:41`). This
matches DD-002 (yolo-by-default with explicit `--safe` override): every
clud-launched backend agent has permissions bypassed unless the user
explicitly asked otherwise. There is no per-subcommand override; the
decision is a single branch at the top of plan construction.

## Unknown-flag passthrough

`args.passthrough` is the bucket clap fills with anything it didn't recognize.
The builder appends it verbatim after the synthesized prompt and before any
launch-mode-specific splices (`builder.rs:176`). Adding a new clud flag means
declaring it in `crates/clud-bin/src/args.rs`; anything not declared falls
through to the backend.

## `--dry-run` contract

`main.rs:233` emits this JSON shape and exits 0:

```json
{
  "command": ["claude", "--dangerously-skip-permissions", "-p", "..."],
  "iterations": 1,
  "backend": "claude",
  "launch_mode": "subprocess",
  "repeat_interval_secs": null,
  "loop_markers": null
}
```

When a loop is active, `loop_markers` becomes `{"done_path": ..., "blocked_path": ...}`.
When `--repeat` is set, `repeat_interval_secs` is a positive integer.
Consumers: the Python integration suite under `tests/`, end-users debugging
their argv, and the hook-health remediator's preflight (it builds a plan,
inspects the command vector, and only then decides whether to spawn).
**Stability contract:** `command[-1]` is always the prompt body for prompt-
bearing invocations. The stream-json splice in `builder.rs:200` is the load-
bearing reason this invariant holds, and downstream tooling depends on it.

## Key types

- `LaunchPlan` — `crates/clud-bin/src/command/types.rs:6`
- `LoopMarkers` — `crates/clud-bin/src/command/types.rs:28`
- `RepeatSchedule` — `crates/clud-bin/src/command/types.rs:34`
- `LaunchMode` — `crates/clud-bin/src/backend.rs:31`
- `Backend` — `crates/clud-bin/src/backend.rs:7`
- `build_launch_plan` — `crates/clud-bin/src/command/builder.rs:24`
- `has_noninteractive_prompt` — `crates/clud-bin/src/command/builder.rs:13`
- `push_prompt` — `crates/clud-bin/src/command/prompts.rs:74`
- `resolve_loop_task` — `crates/clud-bin/src/command/loop_task.rs:29`
- `WorkerLaunchSpec` (daemon wire-format wrapper) — `crates/clud-bin/src/daemon/types.rs:78`

## See also

- [loop-subsystem.md](loop-subsystem.md) — spec → plan → iteration → marker → artifact cycle.
- [daemon-ipc.md](daemon-ipc.md) — how `WorkerLaunchSpec { plan, ... }` rides the wire.
- [`../../crates/clud-bin/src/command/README.md`](../../crates/clud-bin/src/command/README.md) — file-level map of the `command/` submodules.
- [`../DESIGN_DECISIONS.md`](../DESIGN_DECISIONS.md) — DD-002 (YOLO default + `--safe`), DD-005 (single source of truth for backend argv).
