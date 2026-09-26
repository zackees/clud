---
name: grind-land
description: "Land one /grind PR: watch its checks with pr_merge_watch, admin-merge when green, or diagnose the failure and hand it back to the integrator. Never edits or builds."
triggers:
  - When the grind workflow lands a pushed PR
  - When the user asks to watch a PR and merge it once green
disable-model-invocation: true
---
<!-- managed-by: clud -->

# /grind-land

The integrator already proved RED -> GREEN locally; you confirm it on the
server and merge.

1. Watch: `clud tool run github/pr_merge_watch.py <n> --timeout 540` (a plain
   `clud`: clud's hook refuses a `"$CLUD_EXE"` program word).
   Run it bare; its exit code is the result (do not pipe it through `tail`).
   Keep `--timeout` below your 600 s tool-call cap so the watch exits `4`
   itself instead of being orphaned in the background.
2. By exit code:
   - `0` (checks green, mergeable): `gh pr merge <n> --admin --squash
     --delete-branch`. Return `status=merged`.
   - `1` (a required check failed): `gh pr checks <n>` and
     `gh run view <run> --log-failed` for the failing job. Return
     `status=needs_fix` with the failing job and the relevant log lines in
     `failure_log`.
   - `2` (new review activity): read the unresolved comments with
     `gh pr view <n> --comments`. Return `status=needs_fix` with each
     actionable comment in `failure_log`.
   - `3` (closed or merged elsewhere): report it; `status=merged` only if
     `gh pr view` says merged, else `gave_up`.
   - `4` (timeout): watch once more; a second timeout is `gave_up`.
   - `5` (approval required: a fork PR's runs are `action_required`): stop
     and report it to the user; do not retry. Return `status=gave_up`.
   - `6` (never reported: a required check never got a check run although
     every run finished, likely path or branch filters, or a `[skip ci]`
     marker on a branch with required checks): report it as blocked. Return
     `status=gave_up`.
   - `7` (stale: a check is older than 14 days): re-run its workflow once,
     e.g. `gh run rerun <id>`, then watch again; a second `7` is `gave_up`.
   - `8` (NO_CHECKS: no workflows, a `[skip ci]`-style marker on the head
     commit, or no check registered within the grace period): read the
     final line's `mergeStateStatus`. `CLEAN` means merge now, as for `0`.
     Anything else (`BLOCKED`, `BEHIND`, ...): report it and return
     `status=gave_up`.
   - `9` (conflict): when the final reason is `conflict`
     (`mergeable=CONFLICTING` / `mergeStateStatus=DIRTY`), return
     `status=needs_fix` asking the integrator to rebase onto the base branch.
     When it is `mergeable_unknown`, watch once more first; a second `9` is
     `needs_fix`.
   - `10` (GitHub unreachable: auth, rate limit or network; the stderr is
     printed): run `gh auth status`, retry the watch once, then `gave_up`
     with the stderr. Never report it as closed.
   - `11` (queued too long; only with `--max-queued`): report the queued
     jobs and runner labels. Return `status=gave_up`.
   - A cancelled check superseded by a newer run of the same workflow is
     ignored, so concurrency cancellations are not failures.
3. **Feature stage.** A goal PR in the feature stage has the feature branch
   as its base and merges into it only, never into `<main>`: on exit `0`
   use `gh pr merge <n> --admin --merge` instead of step 2's command. No
   `--delete-branch` on any merge while the feature PR is open. Right after a
   goal PR merges into the feature branch, record it so the issue is never
   lost:
   1. `gh label create grind:on-feature --force` (safe when it exists).
   2. `gh issue edit <N> --add-label grind:on-feature` on the goal issue,
      and the same on the meta issue (the feature group's sub-meta in a meta
      of metas) and on the original issue when intake converted it into the
      meta. Adding a label twice is harmless.
   3. `gh issue comment <N> --body '...'` with a readable line (`Landed on
      feature branch <feature-branch> via #<gpr>; closes when feature PR
      #<fpr> merges.`) followed by the hidden marker, exactly
      `<!-- grind:v1 feature-pr=#<fpr> branch=<feature-branch> goal-pr=#<gpr> run=<run-id> -->`.
      On the meta (and original) issue, first read its comments
      (`gh issue view <m> --json comments`); only if none carries a marker
      with `feature-pr=#<fpr>`, post the same marker without `goal-pr`.
   4. `gh pr view <fpr> --json body`, then `gh pr edit <fpr> --body '...'`
      with `Closes #<N>` added and the goals table (goal, goal PR, status)
      updated; keep every other line. You have no file tools, so pass the
      body inline. View the body again and repeat the edit if `Closes #<N>`
      is missing: in parallel mode another lander may have edited it at the
      same moment. Only you add a goal's `Closes` line, and only after its
      goal PR merged, so a goal that never lands never closes.
   Never `gh issue close`: the feature PR's `Closes` lines close the issues
   when it merges into `<main>`, and clud's hook denies the command. The
   feature PR
   itself is merged only when the run's `feature_merge` is `auto`: once
   every feature goal has landed and its checks are green, `gh pr ready
   <feature-pr>` then `gh pr merge <feature-pr> --merge`; never `--admin`,
   `--squash` or `--delete-branch` on it. If required review blocks the
   merge, stop and return "waiting for review". Under `later` or `comment`
   leave the feature PR alone. clud's command hook enforces these rules for
   the lander.
4. You cannot edit or build. The workflow gives a `needs_fix` to the
   integrator and calls you again, up to 10 rounds. Each fix round goes back
   through the integrator, which reruns the run's lint and test scripts
   before pushing.
