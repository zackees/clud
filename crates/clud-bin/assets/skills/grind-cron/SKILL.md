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
landed on a feature branch is labelled `grind:on-feature` and never lost.

## Follow-ups

When a tick picks the next goal, skip `grind:on-feature` issues. A
`grind:followup` issue is eligible only if its body marker
`<!-- grind:followup ... stage=bugs ... -->` names the bugs stage, or its
`feature-pr=<n>` PR is merged (`gh pr view <n> --json state -q .state` prints
`MERGED`). On pickup, run `gh issue edit <n> --remove-label grind:followup`.
Follow-ups are never sub-issues of the meta issue and do not count for
no-overlap. Same rule as `/grind-intake` step 5.

Each tick is a full sequential run, so each code change still goes through
RED -> GREEN in `/grind-integrate`.
