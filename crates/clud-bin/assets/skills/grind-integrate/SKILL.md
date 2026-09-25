---
name: grind-integrate
description: "Integrate one reviewed /grind goal: commit, rebase onto its base, lint, build and test until green, optionally run ci.yml under act, then push and open the PR. The only grind role that builds; runs one at a time."
triggers:
  - When the grind workflow integrates a reviewed goal
  - When the grind lander hands back a failing PR for a fix round
disable-model-invocation: true
---
<!-- managed-by: clud -->

# /grind-integrate

You hold the run's build lock, so nothing else builds while you do.

1. **Branch.** In the checkout given:
   - Parallel: the worktree is already on the goal's branch.
   - Sequential: `git fetch origin <main>`, then
     `git switch -c <branch> origin/<main>`, carrying the worker edits
     (`git stash` first if the switch needs it).
   Commit the goal's files by name (never `git add -A`) with a conventional
   message naming the issue.
2. **Rebase** onto `origin/<main>` after a fresh fetch. A dependent goal's
   dependency has already merged, so `origin/<main>` contains it.
3. **RED -> GREEN.** Run the goal's focused regression test and show it
   fails without the fix (check out the test alone on the base, or cite the
   reproduction), then passes with it.
4. **Verify.** Run the plan's lint, build and test commands. Fix failures by
   editing, commit, and rerun until green. Do not skip or weaken a test.
   **Run scripts.** When the prompt lists the repo's `./lint` / `./test`,
   run lint before test before every push, fix rounds included, after the
   focused test:
   - A script likely to exceed the 600s Bash cap runs with
     `run_in_background`; judge it by its exit code, not by its output.
   - After any fix, rerun both scripts until both are green.
   - If a script fails on untouched `origin/<main>` too, fix it and commit
     that fix separately as `fix: pre-existing lint failure` (or
     `fix: pre-existing test failure`), apart from the goal's commit.
5. **Local CI**, only when on: run the named `ci.yml` job with
   `act -W .github/workflows/ci.yml -j <job> --pull=false`. Do not wrap it
   in `bosn` or start containers yourself.
6. **Review gate.** Run `/clud-review` on source-code changes.
7. **Push and PR.** `git push -u origin <branch>`, then `gh pr create` with
   `Closes #<id>` for an issue goal (never the parent of a meta issue).
   Return `pushed=true` and the PR URL. The lander watches CI; do not wait
   for it here.

**Fix round.** When the prompt carries a failure from the lander: read it,
reproduce locally when possible, fix, rerun the verify commands (and the run
scripts, lint then test), commit and
push to the same branch. Return the same PR URL.

On a failure you cannot fix, return `pushed=false` with the failing command
and its last 60 lines in `failure_log`.
