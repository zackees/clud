---
name: clud-loop
description: Run Codex loop work as in-chat orchestration with a compact ledger; use legacy clud loop processes only when explicitly requested.
triggers:
  - When the user types "/clud-loop" with or without an interval
  - When the user asks for a Codex /loop polyfill or repeated progress loop
  - When the user wants a recurring Codex agent loop with a cadence like 30m
---
<!-- managed-by: clud -->

# /clud-loop

Run loop work in the current Codex chat. The main Codex agent is the single orchestrator: it owns the loop state, chooses each bounded iteration, validates progress, updates the ledger, and decides whether to continue, stop, or ask for input.

Do not run `clud --codex loop` for normal foreground in-chat work. That command creates a separate Codex process/session boundary and is reserved for explicit legacy external automation.

## Code Change Rule

If the loop task performs or delegates a bug fix or feature implementation, require RED -> GREEN: write or identify the focused failing test/repro first, run it to prove the requirement is currently unmet, implement only the scoped change, then rerun that focused signal until it passes before broader lint/test gates.

## Syntax

Accepted user shapes:

- `/clud-loop [interval] [prompt]`
- `/clud-loop 30m check CI and fix new failures`
- `/clud-loop check CI and fix new failures`
- `/clud-loop 30m`
- `/clud-loop`

The cadence is optional and must be the first argument when present. Accept positive integer durations with `s`, `m`, or `h`, for example `30s`, `5m`, `1h`, or `24h`. If the user supplies unsupported natural language or compound durations, normalize to the closest supported form and confirm before scheduling.

## Mode Selection

Choose exactly one mode:

1. **Foreground in-chat work:** Default when there is no cadence and no explicit request for an external process. Use [Foreground In-Chat Orchestration](#foreground-in-chat-orchestration).
2. **Scheduled same-thread work:** Use when the user gives a cadence such as `30m` and wants recurring follow-up. Prefer Codex thread automation over `clud loop --repeat`.
3. **Legacy external process mode:** Use only when the user explicitly asks for legacy behavior, daemon-backed repeat jobs, external/noninteractive automation, or a `clud --codex loop` process.

## Parent Ledger

Use `<git-root>/.clud/loop/LOOP.md` as the compact parent-owned loop ledger and work journal. Resolve the git root with `git rev-parse --show-toplevel`; fall back to the current directory if that fails.

Create or update the file with this shape:

```text
Loop objective:
Acceptance criteria:
Allowed scope:
Forbidden actions:
Max iterations:
Max subagents per iteration:
Stop conditions:
Current iteration:
State ledger:
Last state delta:
Repeated blockers:
```

If the user supplied a prompt, extract the objective, acceptance criteria, allowed scope, forbidden actions, resource limits, and stop conditions into the ledger. If the user did not supply a prompt and the ledger already exists, continue from it. If neither exists, create a maintenance loop: continue unfinished work, check relevant tests/CI/review feedback, and stop when there is nothing actionable.

Keep the ledger compact. Preserve durable state and evidence; delete stale scratch notes that no longer affect the next iteration.

## Foreground In-Chat Orchestration

Use this as the default.

1. Parse the objective, acceptance criteria, allowed scope, forbidden actions, max iterations, max subagents per iteration, and stop conditions.
2. Initialize or update `.clud/loop/LOOP.md`.
3. Before each iteration, check stop conditions.
4. Select one ready work item or a bounded independent batch.
5. Do lightweight work directly in the main chat when subagents are unnecessary.
6. Spawn subagents only as bounded workers for independent tasks. Workers must not own orchestration, scheduler state, or the loop ledger except for the specific state fields they are asked to report.
7. Give each worker a strict packet: objective slice, owned files or read-only scope, forbidden actions, expected evidence, RED -> GREEN requirement when applicable, and the worker output schema below.
8. Forbid every worker from launching recursive agent/process control: no `clud`, no `codex`, no Claude, no `codex exec`, no `codex resume`, no `clud loop`, and no nested worker launches.
9. Wait for structured worker summaries, validate evidence, update the parent ledger, and decide continue/stop/ask.
10. Stop immediately when a stop condition is met or when another iteration would repeat the same action/error/state delta without progress.

Worker output schema:

```yaml
status: DONE | PARTIAL | BLOCKED | FAILED | NOOP
evidence:
summary:
changes_or_findings:
tests_or_checks:
state_delta:
blocker:
next_recommendation:
confidence:
```

## Stop Conditions

- `DONE`: acceptance criteria met and verified.
- `NO_WORK`: no ready work or no new findings after the quiet threshold.
- `BLOCKED`: missing permissions, credentials, external dependency, or the same blocker repeats across attempts.
- `FAILED`: unrecoverable tool/runtime error or repeated failed strategy.
- `RESOURCE_LIMIT`: max iterations, turns, time, or token budget reached.
- `LOOP_DETECTED`: same action, error, or state delta repeats without progress.
- `USER_STOP`: user interrupts or changes direction.

## Scheduled Same-Thread Automation

When the user asks for a cadence, prefer a Codex thread automation with a durable prompt that keeps the same chat as the orchestrator. The automation prompt should:

1. Read `.clud/loop/LOOP.md`.
2. Run exactly one bounded foreground iteration.
3. Use subagents only as bounded workers with the no-recursion packet above.
4. Update the ledger.
5. Stop the scheduled turn after reporting DONE, NO_WORK, BLOCKED, FAILED, RESOURCE_LIMIT, LOOP_DETECTED, or USER_STOP.

If the current Codex surface cannot create thread automations, write the automation-ready prompt and tell the user that scheduling must be set up in the Codex automation surface. Do not start a daemon-backed repeat job unless the user explicitly requests legacy external mode.

## Legacy External Process Mode

Use this only when the user explicitly asks for legacy, external, daemon-backed, noninteractive, or process-runner behavior. Explain that it crosses into a separate Codex process/session and can be harder to coordinate with the current chat.

For a cadence, the legacy command is:

```bash
clud --codex loop --repeat <interval> --loop-count 1 --no-done .clud/loop/LOOP.md
```

Use `--loop-count 1` so each scheduled wake-up is one external Codex turn. `--repeat` schedules the next run after the previous run completes, so slow runs do not overlap. The repeat job runs in the background; show the session id printed by clud and remind the user that `clud list`, `clud logs <id> --follow`, and `clud kill <id>` manage it.

For a one-shot external process, the legacy command is:

```bash
clud --codex loop .clud/loop/LOOP.md
```

In legacy mode, the normal `clud loop` DONE/BLOCKED marker contract controls completion.

## Guardrails

- Do not run `clud --codex loop` for normal foreground in-chat work.
- Do not run interactive `codex` or `codex resume` as the scheduler.
- Do not use `codex exec resume --last` for unattended scheduled work; another Codex run can become "last".
- Do not ask subagents to launch agents, launch `clud`, launch Codex/Claude, own scheduler state, or decide whether the parent loop continues.
- Do not invent completion files. The underlying loop contract uses `.clud/loop/DONE` and `.clud/loop/BLOCKED` when marker mode is active.
- Do not start repeat jobs for the same `.clud/loop/LOOP.md` unless the user explicitly requests legacy external process mode.
- If the user gives a slash command as the loop prompt, expand it to plain instructions or a skill mention that `codex exec` can understand. Do not assume non-interactive Codex parses interactive slash commands.
