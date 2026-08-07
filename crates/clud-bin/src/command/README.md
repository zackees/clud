# command/

Builds the `LaunchPlan` that downstream runners execute: effective-harness-specific argv assembly (`claude` vs `codex`), YOLO/safe-mode injection, subcommand-driven prompt construction (`loop`, `up`, `rebase`, `fix`, `do`), `--repeat` schedule parsing, DONE/BLOCKED marker contract wiring, and Claude `stream-json` progress injection for subprocess-mode loops.

The `LaunchPlan` contract (construction pipeline, consumers, `--dry-run` JSON) is documented at [docs/architecture/launch-plan.md](../../../../docs/architecture/launch-plan.md); the DONE/BLOCKED contract and `--repeat` no-overlap scheduler at [docs/architecture/loop-subsystem.md](../../../../docs/architecture/loop-subsystem.md).
Provider/harness resolution happens before construction and is documented at
[docs/architecture/launch-targets.md](../../../../docs/architecture/launch-targets.md).

## Files

- `mod.rs` — module facade; re-exports the resolved-target production builder, the native compatibility builder, supporting helpers, and the `LaunchPlan` / `LoopMarkers` / `RepeatSchedule` types.
- `builder.rs` — core `build_launch_plan_for_target` orchestrator, the native `build_launch_plan` compatibility wrapper, and repeat/task helpers. Also owns the `--disallowedTools` policy: `bridge_suppresses_plan_mode` strips `EnterPlanMode` on every Codex-provider / Claude-harness launch (the model can otherwise enter plan mode unprompted — see [DD-033](../../../../docs/DESIGN_DECISIONS.md#dd-033-plan-mode-is-disabled-unconditionally-on-the-codex-to-claude-bridge)), `--unattended` / `clud loop` additionally strip `AskUserQuestion`, and `plan_mode_suppression_notice` emits the green TTY-only override hint.
- `loop_task.rs` — resolves the `clud loop` positional (GH issue/PR URL, `#42` shortform, file path, or literal) into prompt text, with `gh`-backed cache under `.clud/loop/`.
- `prompts.rs` — static prompt templates (`FIX_PROMPT`, `GITHUB_FIX_TEMPLATE`, `REBASE_PROMPT`, `UP_PROMPT`) and the backend-aware `push_prompt`, `build_up_prompt`, `build_fix_prompt` builders.
- `types.rs` — `LaunchPlan`, `LoopMarkers`, `RepeatSchedule` serde structs that flow into `--dry-run` JSON and into daemon job records.
- `tests.rs` — 60+ unit tests covering yolo/safe, codex `exec`/`resume`, loop contract injection, stream-json placement before `-p`, `--repeat` parsing edge cases, and scheduler no-overlap invariants.

## Key items

- `build_launch_plan_for_target(args, target, backend_path) -> LaunchPlan` — production path
- `build_launch_plan(args, backend, backend_path) -> LaunchPlan` — native compatibility/test wrapper
- `has_noninteractive_prompt(args) -> bool`
- `parse_repeat_interval(raw) -> Result<u64, String>`
- `repeat_implies_no_done_warning(repeat, no_done, done) -> Option<&'static str>`
- `next_run_at_millis(completed_at_millis, interval_secs) -> u64`
- `summarize_task_name(input, max_chars) -> String`
- `resolve_loop_task(task, git_root, refresh) -> String` — `loop_task.rs`
- `resolve_marker_paths(cwd, git_root, done_override) -> MarkerPaths` — `loop_task.rs`
- `push_prompt(cmd, backend, prompt)` — `prompts.rs`
- `build_up_prompt(message, publish) -> String` — `prompts.rs`
- `build_fix_prompt(url) -> String` — `prompts.rs`
- `struct LaunchPlan` — executable argv plus provider/harness metadata, launch mode, repeat state, task summary, markers, and stream-json state
- `struct LoopMarkers { done_path, blocked_path }`
- `struct RepeatSchedule { interval_secs }`

## Used by

- `main.rs` — calls `build_launch_plan_for_target` and `repeat_implies_no_done_warning` to assemble the resolved plan and emit the `--repeat` warning before dispatch.
- `runner.rs` — consumes `LaunchPlan` to spawn PTY/subprocess and drive iteration loops.
- `loop_check.rs` — reads `plan.loop_markers` to poll DONE/BLOCKED after each iteration.
- `hook_health/prompts.rs` — builds a plan as part of doctor-style health probes.
- `daemon/entry.rs`, `daemon/types.rs` — persist and re-execute `LaunchPlan` records via the daemon worker.
- `loop_artifacts.rs` — references the `chrono_like_now` algorithm pattern from `loop_task.rs`.
