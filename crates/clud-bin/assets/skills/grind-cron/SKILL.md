---
name: grind-cron
description: "/grind cron mode: hand repetition to the harness's native /loop, with each tick running the grind workflow sequentially on the next single issue."
triggers:
  - When /grind runs in cron mode
  - When clud grind starts without an explicit mode
disable-model-invocation: true
---
<!-- managed-by: clud -->

# /grind-cron

Use the prompt's **GitHub repository** (`OWNER/REPO`) as `<github-repo>`. Keep the checkout path separate. For direct use, resolve an explicit GitHub issue/PR URL or `git remote get-url origin` first; report an unresolved identity before running repository commands.

The harness owns repetition. Do not build an external loop, iteration cap,
or DONE/BLOCKED marker; see `docs/architecture/grind.md` in the clud repo.

Issue one `/loop` whose body is:

> Run `clud grind reconcile` first and report its stale lines. Then take the next open goal from <source> that has no open linked PR. If none
> remain, end the loop. Otherwise start the Workflow named `grind-run` with
> `mode: "sequential"`, that one goal, and these recorded answers: <models,
> ci>. When it returns, do `/grind`'s Finish step: no files left behind, and
> no rebase; confirm `git status` is clean. Ask the user nothing: every
> answer was recorded up front. Restore `preflight` (Finish step 4: back
> on the starting branch, `git pull --ff-only` only when that is `<main>`,
> never a rebase) only when the loop ends.

`<source>` is what `/grind-intake` resolved: the meta issue's children, the
issue list, or the repo's issues page. Keep the run facts (the file
`clud grind-facts path` prints) in place for the whole loop, with `mode` set
to `sequential`: every tick runs in this session, so it reads the same file.
Start every tick with `clud grind-facts path`: asking for the path marks the
facts as current, so a loop that outlives the 72-hour staleness cutoff keeps
its caps. Clear them only when the loop ends. Every tick runs
`clud grind reconcile` before picking a goal, so an issue whose goal PR
landed on a feature branch is tracked by its `grind:v1` marker and never lost.

Intake runs once, before the loop. A tick never re-runs `/grind-intake`; it
takes the next goal straight from `<source>`, so a single-issue wrap (a
one-child meta for #N) happens at most once per run and ticks create no meta.

## Follow-ups

When a tick picks the next goal, skip issues an open feature PR's `Closes`
line links. A follow-up issue is eligible only if its body marker
`<!-- grind:followup ... stage=bugs ... -->` names the bugs stage, or its
`feature-pr=<n>` PR is merged (`gh pr view --repo <github-repo> <n> --json state -q .state` prints
`MERGED`). Grind never labels an issue (#1958).
Follow-ups are never sub-issues of the meta issue and do not count for
no-overlap. Same rule as `/grind-intake` step 5.

Each tick is a full sequential run, so each code change still goes through
RED -> GREEN in `/grind-integrate`.

## Publication resumption

For an approved run with a publication-resume manifest, use one native `/loop`
in this same session. Each tick starts with `clud grind-facts path`, reads
`orchestration_resume`, and calls the native Workflow with its saved actual
`scriptPath` and `resumeFromRunId: <saved runId>`. Pass the complete original
approved arguments, changing only `gateEpochs` to the manifest's map.
Graph resumption requires Claude Code >= 2.1.306 with branch-local native
caches. If the hook refuses an older or unidentified runtime, retain
`orchestration_resume` and its original launch references, report the missing
capability, and stop this loop without Finish. Resumption also requires the
native receipt proving the original run used supported caches: upgrading the
harness does not migrate an old run. Never bypass a missing/incompatible
origin receipt or replay completed goals.
Completed native agent calls retain their cache keys; only deferred gate
observations receive new epochs. Do not create a replacement workflow script,
assert completed-goal results, re-run intake, or poll from a shell.

Wait for the native task notification. If it returns another resume manifest,
replace the saved manifest and native launch references with those from this
call, retaining the original approved arguments. Report remaining goals and
their actual evidence. A hard failure is reported as a failure, never relabelled
as publication waiting. When no resume manifest remains, end this loop, remove
`orchestration_resume`, and continue the original router's next stage or Finish.
If native `/loop` or native Workflow resumption is unavailable, report that
specific missing capability and retain the manifest for explicit resumption;
do not invent a notification mechanism or external relaunch loop.

If a publication-resume manifest has `exhausted: true`, stop the native loop,
retain `orchestration_resume`, its launch references and the manifest, and
report the replay-history limit. Do not call Finish, advance its epochs or
restart completed work. The deferred goals remain unfinished.
