# Launch Plan

Every code path that decides "what would clud actually run" funnels through
`command::build_launch_plan_for_target` and consumes the resulting
`LaunchPlan`. The legacy `build_launch_plan` wrapper remains for native
provider/harness compatibility and focused tests; cross-route production code
must pass a resolved target. No other place in the binary reconstructs backend
argv, iteration budget, working directory, repeat schedule, DONE/BLOCKED
marker paths, or stream-json injection.

## The struct

`LaunchPlan` lives in `crates/clud-bin/src/command/types.rs`. Trimmed shape:

```rust
pub struct LaunchPlan {
    pub command: Vec<String>,           // argv: command[0] is the backend exe
    pub iterations: u32,                // 1 for one-shot; >1 for `clud loop`
    pub backend: Backend,               // Claude | Codex
    pub model_provider: Option<ModelProvider>,
    pub requested_harness: Option<HarnessSelection>,
    pub effective_harness: Option<Backend>,
    pub provider_source: Option<PreferenceSource>,
    pub harness_source: Option<PreferenceSource>,
    pub launch_mode: LaunchMode,        // Subprocess | Pty
    pub cwd: Option<String>,            // snapshot of std::env::current_dir()
    pub repeat_schedule: Option<RepeatSchedule>, // Some(interval_secs) iff --repeat
    pub task_summary: Option<String>,   // short label for session name
    pub loop_markers: Option<LoopMarkers>,       // DONE/BLOCKED absolute paths
    pub stream_json_progress: bool,     // claude subprocess-mode loop only
}
```

`LoopMarkers { done_path, blocked_path }` and
`RepeatSchedule { interval_secs }` live in the same module. All three derive
`Serialize` / `Deserialize` so the complete plan round-trips through the
daemon's `WorkerLaunchSpec` (`crates/clud-bin/src/daemon/types.rs`).
`--dry-run` instead emits a stable, user-facing JSON projection built from the
same plan.

The five provider/harness fields are additive and deserialize to `None` for an
old payload. `LaunchPlan::model_provider()` and `effective_harness()` fall back
to the legacy `backend`, preserving native-provider behavior.

## Construction pipeline

`build_launch_plan_for_target(args, target, backend_path) -> LaunchPlan` is the
production entrypoint in `crates/clud-bin/src/command/builder.rs`. In order, it:

1. **Seeds `cmd` with `backend_path`** and reads the effective harness from
   the resolved target.
2. **Adds Codex configuration before its subcommand.** Every configured `-c`
   override is emitted first, followed by the project-document fallback when
   the caller did not override it.
3. **Selects the Codex subcommand.** Non-interactive prompts use `exec`;
   continuation requests use `resume`. Claude has no corresponding
   sub-keyword.
4. **Adds common launch options.** The builder injects the harness-specific
   YOLO flag unless `--safe`, emits `--model`/`-m`, and appends Codex
   `resume --last` for `--continue`.
5. **Builds the selected task.** For `loop`, repeat duration is parsed first,
   then DONE/BLOCKED marker policy and paths are resolved, the task text is
   loaded, the marker contract is appended, and the prompt is pushed. `up`,
   `rebase`, and `fix` use their prompt builders; a direct launch handles
   prompt/message/continue/resume arguments in harness-specific form.
6. **Forwards unknown flags** from `args.passthrough` after task-specific
   arguments.
7. **Resolves launch mode** from `--pty`/`--subprocess`, the effective harness,
   whether Codex uses `exec`, loop state, and parent-TTY detection.
8. **Injects stream-json progress flags** for Claude subprocess-mode loops.
   They are spliced immediately before `-p` so the prompt remains at
   `command[-1]`.
9. **Finalizes the plan** with provider/harness/source metadata, cwd,
   repeat/marker/task state, graphics settings, and stream-json state.

## Consumers

Every code path that runs (or describes) the resolved argv reads from a
`LaunchPlan`:

- `crates/clud-bin/src/main.rs` — `build_launch_plan_for_target` is called
  once after provider/harness resolution.
- `crates/clud-bin/src/main.rs` — `--dry-run` JSON emission (see contract
  below); exits 0 without spawning.
- `crates/clud-bin/src/runner.rs` (`run_plan_subprocess` and `run_plan_pty`) —
  per-iteration child
  spawn, reading `plan.command`, `plan.cwd`, `plan.iterations`, and
  `plan.stream_json_progress`.
- `crates/clud-bin/src/daemon/entry.rs` — `run_centralized_session` clones
  the plan into a `WorkerLaunchSpec` and ships it over IPC.
- `crates/clud-bin/src/daemon/worker.rs` — worker process
  re-spawns the backend using `spec.plan.command` and `spec.plan.cwd`.
- `crates/clud-bin/src/hook_health/prompts.rs` — `run_backend_prompt` carries
  the resolved launch target into `build_launch_plan_for_target` and runs the
  resulting argv as a one-shot subprocess for hook-migration prompting
  (`--fix-hooks`).
- `crates/clud-bin/src/loop_artifacts.rs` — `LoopSession::start` consumes
  `plan.iterations` to seed `TaskInfo::total_iterations` written to
  `<git-root>/.clud/loop/info.json`.

## Effective-harness-specific divergence

The legacy `Backend` enum now identifies the executable harness in plan
construction. Model-provider selection is carried separately by
`ResolvedLaunchTarget`.

| Concern | Claude harness | Codex harness |
|---|---|---|
| Subcommand keyword | (none) | `exec` for non-interactive prompt; `resume` for `-c`/`--resume` |
| YOLO flag | `--dangerously-skip-permissions` | `--dangerously-bypass-approvals-and-sandbox` |
| Model flag | `--model <id>` | `-m <id>` |
| Prompt delivery | `-p <prompt>` | bare positional |
| `-m <message>` | `-m <message>` passthrough | dropped because it would clobber `--model` |
| `--continue` | `--continue` | `resume --last` |
| `--resume <id>` | `--resume <id>` | `resume <id>` positional |
| Stream-json progress | `--output-format stream-json --verbose` injected before `-p` for subprocess-mode loops | not exposed by codex; skipped |

## YOLO injection

YOLO is on by default. The `--safe` flag is the opt-out — when set,
`build_launch_plan_for_target` skips the YOLO push entirely. This
matches DD-002 (yolo-by-default with explicit `--safe` override): every
clud-launched backend agent has permissions bypassed unless the user
explicitly asked otherwise. There is no per-subcommand override; the
decision is a single branch at the top of plan construction.

## Unknown-flag passthrough

`args.passthrough` is the bucket clap fills with anything it didn't recognize.
The builder appends it verbatim after the synthesized prompt and before any
launch-mode-specific splices. Adding a new clud flag means
declaring it in `crates/clud-bin/src/args.rs`; anything not declared falls
through to the backend.

## `--dry-run` contract

`main.rs` emits this JSON shape and exits 0:

```json
{
  "command": ["claude", "--dangerously-skip-permissions", "-p", "..."],
  "iterations": 1,
  "backend": "claude",
  "model_provider": "claude",
  "requested_harness": "default",
  "effective_harness": "claude",
  "provider_source": "built_in_default",
  "harness_source": "built_in_default",
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
bearing invocations. The stream-json splice in `builder.rs` is the load-
bearing reason this invariant holds, and downstream tooling depends on it.

## Key types

- `LaunchPlan`, `LoopMarkers`, `RepeatSchedule` —
  `crates/clud-bin/src/command/types.rs`
- `ModelProvider`, `HarnessSelection`, `ResolvedLaunchTarget`, `LaunchMode`,
  `Backend` — `crates/clud-bin/src/backend.rs`
- `build_launch_plan_for_target` (production),
  `build_launch_plan` (native compatibility/test),
  `has_noninteractive_prompt`, `parse_repeat_interval` —
  `crates/clud-bin/src/command/builder.rs`
- `push_prompt` — `crates/clud-bin/src/command/prompts.rs`
- `resolve_loop_task` — `crates/clud-bin/src/command/loop_task.rs`
- `WorkerLaunchSpec` (daemon wire-format wrapper) —
  `crates/clud-bin/src/daemon/types.rs`

## See also

- [loop-subsystem.md](loop-subsystem.md) — spec → plan → iteration → marker → artifact cycle.
- [daemon-ipc.md](daemon-ipc.md) — how `WorkerLaunchSpec { plan, ... }` rides the wire.
- [`../../crates/clud-bin/src/command/README.md`](../../crates/clud-bin/src/command/README.md) — file-level map of the `command/` submodules.
- [`../DESIGN_DECISIONS.md`](../DESIGN_DECISIONS.md) — DD-002 (YOLO default + `--safe`), DD-005 (single source of truth for backend argv).
