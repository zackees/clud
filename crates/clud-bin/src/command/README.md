# command/

Builds the `LaunchPlan` that downstream runners execute: backend-specific argv assembly (`claude` vs `codex`), YOLO/safe-mode injection, subcommand-driven prompt construction (`loop`, `up`, `rebase`, `fix`), `--repeat` schedule parsing, DONE/BLOCKED marker contract wiring, and Claude `stream-json` progress injection for subprocess-mode loops.

The `LaunchPlan` contract (construction pipeline, consumers, `--dry-run` JSON) is documented at [docs/architecture/launch-plan.md](../../../../docs/architecture/launch-plan.md); the DONE/BLOCKED contract and `--repeat` no-overlap scheduler at [docs/architecture/loop-subsystem.md](../../../../docs/architecture/loop-subsystem.md).

## Files

- `mod.rs` — module facade; re-exports `build_launch_plan`, `has_noninteractive_prompt`, `next_run_at_millis`, `repeat_implies_no_done_warning`, `summarize_task_name`, and the `LaunchPlan` / `LoopMarkers` / `RepeatSchedule` types.
- `builder.rs` — core `build_launch_plan` orchestrator plus `parse_repeat_interval`, `repeat_implies_no_done_warning`, `next_run_at_millis`, and `summarize_task_name` helpers.
- `loop_task.rs` — resolves the `clud loop` positional (GH issue/PR URL, `#42` shortform, file path, or literal) into prompt text, with `gh`-backed cache under `.clud/loop/`.
- `prompts.rs` — static prompt templates (`FIX_PROMPT`, `GITHUB_FIX_TEMPLATE`, `REBASE_PROMPT`, `UP_PROMPT`) and the backend-aware `push_prompt`, `build_up_prompt`, `build_fix_prompt` builders.
- `types.rs` — `LaunchPlan`, `LoopMarkers`, `RepeatSchedule` serde structs that flow into `--dry-run` JSON and into daemon job records.
- `tests.rs` — 60+ unit tests covering yolo/safe, codex `exec`/`resume`, loop contract injection, stream-json placement before `-p`, `--repeat` parsing edge cases, and scheduler no-overlap invariants.

## Key items

- `build_launch_plan(args, backend, backend_path) -> LaunchPlan` — `builder.rs:24`
- `has_noninteractive_prompt(args) -> bool` — `builder.rs:13`
- `parse_repeat_interval(raw) -> Result<u64, String>` — `builder.rs:250`
- `repeat_implies_no_done_warning(repeat, no_done, done) -> Option<&'static str>` — `builder.rs:290`
- `next_run_at_millis(completed_at_millis, interval_secs) -> u64` — `builder.rs:316`
- `summarize_task_name(input, max_chars) -> String` — `builder.rs:320`
- `resolve_loop_task(task, git_root, refresh) -> String` — `loop_task.rs:29`
- `resolve_marker_paths(cwd, git_root, done_override) -> MarkerPaths` — `loop_task.rs:8`
- `push_prompt(cmd, backend, prompt)` — `prompts.rs:74`
- `build_up_prompt(message, publish) -> String` — `prompts.rs:86`
- `build_fix_prompt(url) -> String` — `prompts.rs:115`
- `struct LaunchPlan` (command, iterations, backend, launch_mode, cwd, repeat_schedule, task_summary, loop_markers, stream_json_progress) — `types.rs:6`
- `struct LoopMarkers { done_path, blocked_path }` — `types.rs:28`
- `struct RepeatSchedule { interval_secs }` — `types.rs:34`

## Used by

- `main.rs` — calls `build_launch_plan` and `repeat_implies_no_done_warning` to assemble the plan and emit the `--repeat` warning before dispatch.
- `runner.rs` — consumes `LaunchPlan` to spawn PTY/subprocess and drive iteration loops.
- `loop_check.rs` — reads `plan.loop_markers` to poll DONE/BLOCKED after each iteration.
- `hook_health.rs` — builds a plan as part of doctor-style health probes.
- `daemon/entry.rs`, `daemon/types.rs` — persist and re-execute `LaunchPlan` records via the daemon worker.
- `loop_artifacts.rs` — references the `chrono_like_now` algorithm pattern from `loop_task.rs`.
