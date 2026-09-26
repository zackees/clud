---
name: grind-integrate
description: "Integrate one reviewed /grind goal: commit, rebase onto its base, lint, build and test until green, optionally run ci.yml under act, then push and open the PR, or park a failed sequential goal's changes on a local wip branch. The only grind role that builds; runs one at a time."
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
   - Sequential: `git fetch origin <base>`, then
     `git switch -c <branch> origin/<base>` (`<base>` as in step 2), carrying the worker edits
     (`git stash` first if the switch needs it).
   Commit the goal's files by name (never `git add -A`) with a conventional
   message naming the issue.
2. **Rebase** onto `origin/<base>` after a fresh fetch, where `<base>` is
   `<main>` in the bug stage and the feature branch
   (`grind/meta-<M>-<run-id>`) in the feature stage. A dependent goal's
   dependency has already merged, so `origin/<base>` contains it. In the
   feature stage, first check whether `origin/<main>` moved past the feature
   branch; if so, in the feature worktree fast-forward to the feature
   branch's origin (`git merge --ff-only origin/<feature>`), merge (never
   rebase) `origin/<main>` into it with `git merge --no-ff`, and push it with
   a plain push, then rebase the goal onto the updated feature branch.
3. **RED -> GREEN.** Run the goal's focused regression test and show it
   fails without the fix (check out the test alone on the base, or cite the
   reproduction), then passes with it.
4. **Verify.** Run the plan's lint, build and test commands. Fix failures by
   editing, commit, and rerun until green. Do not skip or weaken a test.
   **Run must_verify.** The reviewer cannot run anything, so the checks it
   lists under `must_verify` arrive in your verify commands: run every one
   before pushing. A failure there is a real defect: fix it.
   **Run scripts.** When the prompt lists the repo's `./lint` / `./test`,
   run lint before test before every push, fix rounds included, after the
   focused test:
   - A script likely to exceed the 600s Bash cap runs with
     `run_in_background`; judge it by its exit code, not by its output.
   - After any fix, rerun both scripts until both are green.
   - If a script fails on untouched `origin/<main>` too, fix it and commit
     that fix separately as `fix: pre-existing lint failure` (or
     `fix: pre-existing test failure`), apart from the goal's commit.
   **After a failed run: read, then decide.** A deterministic failure does
   not go away when rerun, so:
   - **Read before retrying.** After any failed lint or test run, read the
     failing test names and their errors first. Never rerun a suite you have
     not read the failure of.
   - **Retry only infrastructure errors.** Rerun unchanged only for a known
     infrastructure error (for example `SESSION relay closed before Exit`,
     or a network or registry timeout), at most twice. Never wrap a full
     suite in a retry loop (`for i in 1 2 3; do bash test; done`); clud's
     hook refuses one.
   - **Same failure twice is real.** If the same test fails on two
     consecutive runs, it is not flaky: fix it, or report it as pre-existing
     with evidence, namely that it also fails on untouched `origin/<main>`.
   - **Wait by condition.** Wait on a background run by its exit (the
     completion notice, or an exit code or marker file it writes), never in
     fixed 540–600 s sleep blocks.
5. **Local CI**, only when on: run the named `ci.yml` job with
   `act -W .github/workflows/ci.yml -j <job> --pull=false`. Do not wrap it
   in `bosn` or start containers yourself.
6. **Review gate.** Run `/clud-review` on source-code changes.
7. **Push and PR.** `git push -u origin <branch>`, then `gh pr create` with
   `Closes #<id>` for an issue goal only when the PR's base is the
   repository's default branch; when the base is any other branch (e.g. a
   feature branch) use `Refs #<id>` instead, because GitHub only auto-closes
   on merges into the default branch and a premature close would lose the
   issue. Either keyword names the goal's own issue, never the parent of a
   meta issue. So feature-stage goal PRs (base = the feature branch) use
   `Refs #N`. Do not edit the feature PR: the lander adds the goal's
   `Closes #N` line to it only after the goal PR merges into the feature
   branch, so a goal that never lands is never closed by the feature merge.
   Return `pushed=true` and the PR URL. The lander watches CI; do not wait
   for it here.

**Fix round.** When the prompt carries a failure from the lander: read it,
reproduce locally when possible, fix, rerun the verify commands (and the run
scripts, lint then test), commit and
push to the same branch. Return the same PR URL.

On a failure you cannot fix, return `pushed=false` with the failing command
and its last 60 lines in `failure_log`.

**Park (sequential mode, when the prompt says `PARK goal`).** The goal was
rejected, blocked or failed before its work was pushed, and its changes must
leave the shared checkout before the next goal starts. Do not commit to the
goal branch, push, open a PR, or run lint or test.

1. In the checkout given, run `git status --porcelain`. The goal's changes
   are the goal files the prompt lists plus any other path changed since the
   run started. The user's pre-run state, as the prompt's preflight records
   it, is never the goal's: with `carry`, the carried changes in the feature
   worktree stay put. With `stash`, `wip` or none recorded, the checkout was
   clean when the run started.
2. If any of the goal's paths are changed: `git switch -c <park branch>`
   (the uncommitted changes come along; from a goal branch its commits do
   too), `git add -- <those paths>` by name, never `git add -A`, then
   `git commit -m "wip(grind): park goal <id>"`. The park branch
   `wip/grind-<goal>` stays local: never push it.
3. `git fetch origin <base>`, then `git switch --detach origin/<base>`.
   Never `git reset --hard`, `git clean`, `git checkout -- .` or
   `git stash drop`: they destroy what is not the goal's.
4. Run `git status --porcelain` again. Return `parked` (whether you
   committed to the park branch), `branch`, `files`, and `clean=true` only
   when none of the goal's paths is still listed.
