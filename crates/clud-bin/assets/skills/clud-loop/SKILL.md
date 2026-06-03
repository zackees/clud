---
name: clud-loop
description: Polyfill Claude-style /loop behavior for Codex by running clud loop jobs from a shared .clud/loop/LOOP.md task file.
triggers:
  - When the user types "/clud-loop" with or without an interval
  - When the user asks for a Codex /loop polyfill
  - When the user wants a recurring Codex agent loop with an interval like 30m
---
<!-- managed-by: clud -->

# /clud-loop

Emulate Claude Code-style `/loop` for Codex. Codex does not document arbitrary top-level custom slash command registration, so this skill is the clud polyfill: keep the durable work in `.clud/loop/LOOP.md`, then drive Codex through `clud --codex loop`.

## Code Change Rule

If the loop task performs or delegates a bug fix or feature implementation, require RED -> GREEN: write or identify the focused failing test/repro first, run it to prove the requirement is currently unmet, implement only the scoped change, then rerun that focused signal until it passes before broader lint/test gates.

## Syntax

Accepted user shapes:

- `/clud-loop [interval] [prompt]`
- `/clud-loop 30m check CI and fix new failures`
- `/clud-loop check CI and fix new failures`
- `/clud-loop 30m`
- `/clud-loop`

The interval is optional and must be the first argument when present. Use the duration syntax supported by `clud loop --repeat`: positive integer plus `s`, `m`, or `h`, for example `30s`, `5m`, `1h`, or `24h`. If the user supplies unsupported natural language or compound durations, normalize to the closest supported form and confirm before launching.

## Shared Task File

Always use `<git-root>/.clud/loop/LOOP.md` as the working prompt and journal.

1. Resolve the git root with `git rev-parse --show-toplevel`; fall back to the current directory if that fails.
2. Create `.clud/loop/` if needed.
3. If the user supplied a prompt, write or update `.clud/loop/LOOP.md` with:
   - `# clud-loop`
   - `## Objective`
   - `## Cadence`
   - `## Current Status`
   - `## Working Notes`
   - `## Stop Conditions`
4. If the user did not supply a prompt and `.clud/loop/LOOP.md` already exists, use it.
5. If the user did not supply a prompt and no file exists, create a maintenance prompt: continue unfinished work, check relevant tests/CI/review feedback, update the working notes, and stop when there is nothing actionable.
6. Tell any worker or repeated Codex turn to read `.clud/loop/LOOP.md` first, follow the RED -> GREEN code-change rule for fixes/features, and update `Current Status` / `Working Notes` before finishing.

## Interval Mode

When an interval is present, start a daemon-backed repeat job and return control to the user:

```bash
clud --codex loop --repeat <interval> --loop-count 1 --no-done .clud/loop/LOOP.md
```

Use `--loop-count 1` so each scheduled wake-up is one Codex turn. `--repeat` schedules the next run after the previous run completes, so slow runs do not overlap. The repeat job runs in the background; show the session id printed by clud and remind the user that `clud list`, `clud logs <id> --follow`, and `clud kill <id>` manage it.

If the task becomes complete during a scheduled run, write the completion summary into `.clud/loop/LOOP.md`. Do not keep making edits on later wake-ups when the stop condition is already satisfied; report the idle state instead.

## No-Interval Mode

When no interval is present, prefer to keep the main agent focused:

1. If Codex subagents are available, spawn a worker subagent for the loop work. Give it the absolute path to `.clud/loop/LOOP.md`, tell it to read and update that file, and instruct it to run:

   ```bash
   clud --codex loop .clud/loop/LOOP.md
   ```

2. If subagents are unavailable or the user asks for foreground execution, run the same command directly.
3. The normal `clud loop` DONE/BLOCKED marker contract controls completion in no-interval mode.

## Guardrails

- Do not run interactive `codex` or `codex resume` as the scheduler. Use `clud --codex loop`, which routes prompt execution through `codex exec`.
- Do not use `codex exec resume --last` for unattended scheduled work; another Codex run can become "last".
- Do not invent completion files. The underlying loop contract uses `.clud/loop/DONE` and `.clud/loop/BLOCKED` when marker mode is active.
- Do not start multiple repeat jobs for the same `.clud/loop/LOOP.md` unless the user explicitly asks for parallel schedules.
- If the user gives a slash command as the loop prompt, expand it to plain instructions or a skill mention that `codex exec` can understand. Do not assume non-interactive Codex parses interactive slash commands.
