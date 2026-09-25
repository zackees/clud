---
name: grind
description: "Work through a list of goals (a meta issue, issue list, free-form goal, or the repo's issues page) to merged PRs: plan, write, review, integrate and land, in parallel worktrees, sequentially in the local checkout, or one issue per /loop tick. Router for the grind-* skills and the bundled grind workflow."
triggers:
  - When the user runs /grind or clud grind
  - When clud do or /goal has several independent deliverables to implement
  - When the user asks to burn down a meta issue, an issue list, or the issues page
---
<!-- managed-by: clud -->

# /grind

Router for the grind DAG. It gathers every answer up front, then hands the
deterministic part to the bundled `grind-run` workflow, which cannot ask
questions once it runs.

```
/grind ─ grind-intake ─ questions ─┬─ parallel   ─┐
                                   ├─ sequential ─┼─ workflow grind-run: plan → work → review → integrate → land
                                   └─ cron ─ /grind-cron ─ /loop: sequential run, one issue per tick
```

Every code change keeps a RED -> GREEN focused regression: the integrator
first shows the failure or reproduction, then makes it pass before the
repository's broader gates.

## 0. Harness

`/grind` needs Claude Code's Workflow tool and the bundled `grind-*` agent
types. If either is missing (a Codex or other native harness), stop and say:
"/grind requires the Claude harness; relaunch with `--harness claude`." Do
not emulate the workflow by hand.

## 1. Intake

Invoke `/grind-intake` with the user's argument. It returns the goal list
(`[{id, title, brief}]`) in order, plus the repository root and its default
branch (`main` or `master`).

## 2. Questions

Ask with AskUserQuestion, in two calls.

First call:

- **Mode.** (i) *Parallel*: each goal gets a git worktree; workers only read
  and write files; one integrator at a time rebases, lints, builds and tests.
  Uses GitHub's server-side concurrency. (ii) *Sequential*: one goal at a
  time from `origin/<main>` in the local checkout, no worktrees or sister
  clones; best for C++/Rust or other heavy repos where a cold worktree build
  is expensive. (iii) *Cron*: one issue per `/loop` tick, each tick a
  sequential run.
- **Planner model**, **Worker model**, **Reviewer model**: offer the session's
  current model first, labelled "(Recommended)", then the other tiers the
  harness accepts. The session model is whatever this session is running on
  (for `clud --deepseek` or another provider, the model that route resolved);
  never assume a fixed model name.

Second call:

- **Integrator model** (the lander shares it), same options.
- **Local CI**, asked only when both hold:
  - `docker info` succeeds. Otherwise print
    "Docker/github actions disabled due to no docker running" and set CI off.
  - `.github/workflows/ci.yml` exists. Otherwise print
    "No .github/workflows/ci.yml; local GitHub Actions run skipped" and set CI off.

  The question: run the relevant `ci.yml` job locally under `act` before
  each push?

## 3. Record the run

Write `.clud/grind/run.json` at the repository root:

```json
{"mode": "parallel", "ci": false}
```

clud's command hook reads it to apply the per-role caps (the planner may add
worktrees only in parallel mode; the integrator may run `act` only when CI is
on). Delete it when the run ends.

## 4. Run

- Cron: invoke `/grind-cron` with the goal source and the answers.
- Parallel or sequential: start the Workflow named `grind-run` with args
  `{repo, main, mode, goals, ci, models: {planner, worker, reviewer,
  integrator}}`. Omit a model the user left at the session default.

Concurrency is fixed by the workflow: at most 4 planner/worker/reviewer
agents at once, and exactly one integrator.

## 5. Finish

Report each goal's PR and whether it merged; list anything left open with
its reason. Then fetch and rebase the local checkout onto `origin/<main>`,
remove `.clud/grind/run.json`, and confirm `git status` is clean. Remove a
worktree only after checking it has no unpushed work (see `/clud-git`).

## The DAG

| Node | Role | Caps |
|---|---|---|
| `/grind-intake` | router | main session |
| `/grind-plan` | `grind-planner` | read-only git/gh; `git worktree add` in parallel mode |
| `/grind-work` | `grind-worker` | read/write assigned files; read-only `gh`; web search |
| `/grind-review` | `grind-reviewer` | same as worker |
| `/grind-integrate` | `grind-integrator` | full shell, one at a time; no `bosn`, containers only via `act`, no new worktrees |
| `/grind-land` | `grind-lander` | `gh pr`, `pr_merge_watch`, `git push`; no edits |
| `/grind-cron` | router | main session |

Tools are capped by each agent type's `tools:` list; shell commands by
clud's PreToolUse hook, keyed on the agent type.
