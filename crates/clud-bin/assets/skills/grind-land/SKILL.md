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

1. Watch: `"$CLUD_EXE" tool run github/pr_merge_watch.py <n>`. Run it
   bare; its exit code is the result (do not pipe it through `tail`).
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
     every run finished, likely path or branch filters): report it as
     blocked. Return `status=gave_up`.
   - `7` (stale: a check is older than 14 days): re-run its workflow once,
     e.g. `gh run rerun <id>`, then watch again; a second `7` is `gave_up`.
   - A cancelled check superseded by a newer run of the same workflow is
     ignored, so concurrency cancellations are not failures.
3. You cannot edit or build. The workflow gives a `needs_fix` to the
   integrator and calls you again, up to 10 rounds. Each fix round goes back
   through the integrator, which reruns the run's lint and test scripts
   before pushing.
