---
name: grind-cron
description: "/grind cron mode: hand repetition to the harness's native /loop, with each tick running the grind workflow sequentially on the next single issue."
triggers:
  - When /grind runs in cron mode
  - When clud grind starts without an explicit mode
---
<!-- managed-by: clud -->

# /grind-cron

The harness owns repetition. Do not build an external loop, iteration cap,
or DONE/BLOCKED marker; see `docs/architecture/grind.md` in the clud repo.

Issue one `/loop` whose body is:

> Take the next open goal from <source> that has no open linked PR. If none
> remain, end the loop. Otherwise start the Workflow named `grind` with
> `mode: "sequential"`, that one goal, and these recorded answers: <models,
> ci>. When it returns, rebase the checkout onto `origin/<main>` and confirm
> `git status` is clean.

`<source>` is what `/grind-intake` resolved: the meta issue's children, the
issue list, or the repo's issues page. Keep `.clud/grind/run.json` in place
for the whole loop, with `mode` set to `sequential`.

Each tick is a full sequential run, so each code change still goes through
RED -> GREEN in `/grind-integrate`.
