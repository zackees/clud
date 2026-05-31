---
name: clud-pr-merge
description: Wait for CI + CodeRabbit on the current PR, fix regressions and review comments, then merge to main.
triggers:
  - When the user asks to merge a PR
  - When the user says "/clud-pr-merge", "ship it", "land this PR"
  - After /clud-pr completes and the user wants to follow through to merge
---
<!-- managed-by: clud -->

# /clud-pr-merge

Take an open PR from "tests running" to "merged into main". Two gates:

1. **CI gate** — all required checks green, OR every failing check was already failing on main (no regressions introduced by this PR).
2. **Review gate** — no unresolved CodeRabbit comments.

If the gates fail in fixable ways, dispatch a sub-agent to fix; up to 3 rounds total. After 3, give up and surface the blockers.

## Workflow

1. **Resolve the PR.** `gh pr view --json number,headRefName,baseRefName,state,mergeable,statusCheckRollup,url` against the current branch. Refuse if state ≠ OPEN or mergeable ≠ MERGEABLE/UNKNOWN.

2. **Estimate the wait, probe CodeRabbit presence, announce both, then poll.** Before sleeping:
   - **Probe whether CodeRabbit is installed on this repo (issue #194).** Sample the 5 most-recent *merged* PRs for any `coderabbitai[bot]` review:
     ```
     CR_REVIEW_COUNT=$(gh pr list --repo <owner/repo> --state merged --limit 5 --json number \
       | jq -r '.[].number' \
       | while read n; do
           gh api repos/<owner/repo>/pulls/$n/reviews \
             --jq '[.[] | select(.user.login=="coderabbitai[bot]")] | length'
         done | awk '{s+=$1} END {print s+0}')
     if [ "$CR_REVIEW_COUNT" -eq 0 ]; then CR_REQUIRED=false; else CR_REQUIRED=true; fi
     ```
     - Sample "merged" not "any state" — open PRs may legitimately predate CR's review window and would yield a false negative.
     - Sample size 5 — robust against one slipped PR, cheap (one cluster of API calls), no caching across runs (CR may be installed mid-session and caching would shadow that).
     - "Drop" not "warn and continue" — when CR is provably absent, requiring a CR review is a *category* error, not a relaxed gate.
   - Probe the repo's *recent* CI duration: `gh run list --repo <owner/repo> --branch main --limit 10 --json conclusion,startedAt,updatedAt`. Take the p90 wall-clock; round up to the nearest 5 min. This is your expected wait.
   - **Default cap: 45 minutes** (the FastLED repo and similar embedded projects routinely hit 25–30 min platform builds). Tighten the cap to `max(15, p90 + 5min)` for repos with faster CI; never go below 15 minutes.
   - **Announce upfront** with one user-facing line that names both the cap and the CodeRabbit decision. Silent polling reads as a hung shell — never skip this announcement.
     - CR detected: `[clud-pr-merge] polling CI for up to <N> min; slowest recent build was <P90> min; CodeRabbit detected — gating on CI + CR review.`
     - CR not detected: `[clud-pr-merge] polling CI for up to <N> min; slowest recent build was <P90> min; CodeRabbit not detected on this repo (sampled 5 recent merged PRs) — gating on CI only.`
   - Then loop with **30-second sleeps**. Each iteration fetches:
     - `gh pr view <num> --json state --jq .state` — **first** check every iteration. If the PR became MERGED or CLOSED, **break the loop immediately** (success for MERGED, abort for CLOSED). See "Early-exit on PR state change" below.
     - `gh pr checks <num> --json name,state,bucket` — every required check has a non-PENDING state.
     - **Only if `CR_REQUIRED=true`:** `gh api repos/{owner}/{repo}/pulls/{num}/reviews --jq '.[] | select(.user.login=="coderabbitai[bot]")'` — at least one CodeRabbit review has been posted (or CodeRabbit explicitly "skipped" the review, which counts as reported). When `CR_REQUIRED=false`, skip this check entirely — the exit condition collapses to `pending == 0`.

   Exit condition:
   - `CR_REQUIRED=true` → `pending == 0 AND coderabbit_reviews != 0` (current behavior; if CR was sampled-present but never reviews this specific PR within the cap, the skill times out — that's correct, an installed-but-slow CR is still a real gate).
   - `CR_REQUIRED=false` → `pending == 0` alone.
   - In either case, a PR state change to MERGED/CLOSED also breaks the loop.

   If the cap elapses with no signal, **give up and warn the user**: `[clud-pr-merge] timed out waiting for CI/CodeRabbit after <N> minutes. Re-run when checks have reported.` Do NOT merge.

   **Early-exit on PR state change (CRITICAL — prevents the "stuck shell" bug).** GitHub Actions jobs do NOT cancel when a PR is merged out from under them. If a maintainer merges (or auto-merge fires) while the poll is waiting, the slow platform-build jobs on the original SHA keep running for another ~20 min. The poll's "no PENDING checks" condition stays false the whole time, so the loop keeps spawning sleeps and never exits — from the user's perspective the shell is hung. Two preventions, both mandatory:
   1. Check `gh pr view <num> --json state` FIRST in every poll iteration and break the loop the moment state ≠ OPEN. The poll's exit reason is "PR merged/closed," not "all checks finished."
   2. For polls started via `run_in_background: true`, the bash loop runs OUT of band from the conversation. The moment merge is confirmed externally (manual `gh pr view`, the `<task-notification>` payload of the bg task, or any other signal), kill the background task via the runtime's stop mechanism. Do NOT rely on "it'll self-terminate" — the loop's purpose is moot but the loop body doesn't know that.

   **Polling pattern.** Use Bash with `until <condition>; do sleep 30; done` (sleep *inside* the body). Do not chain `sleep N; <cmd>` — the Claude Code harness blocks leading sleeps. For waits longer than ~5 minutes consider `ScheduleWakeup` instead so the conversation isn't held open by an active poll.

   **`gh pr checks` exit-code gotcha.** `gh pr checks` exits non-zero whenever ANY check has failed, even with `--json`. A backgrounded poll that finishes with exit 1 may still have produced complete output — always read the output, never gate on `$?`.

3. **Diff CI failures vs main (with PR-only-workflow handling).** For every failing check on the PR:
   - Fetch the same check name on `main`'s latest commit: `gh api repos/{owner}/{repo}/commits/main/check-runs`.
   - **Same check failing on main** → **pre-existing failure**, not a regression. Mark and skip.
   - **Same check passing on main** → **regression**. Add to the fix queue.
   - **Same check absent from main's check-runs** → the workflow is PR-only (e.g. `pull_request_target` or `pull_request` event triggers don't run on `push` to main). Classify by sampling recent PRs: `gh pr list --state all --limit 10 --json number` then `gh pr checks <n>` for each. If the same check fails on every recent PR, treat as **pre-existing infrastructure failure** and skip. If failures are sporadic, treat as **inconclusive** and surface to the user for judgment — do NOT silently merge.

4. **Collect CodeRabbit findings.** `gh api repos/{owner}/{repo}/pulls/{num}/comments` filtered to `user.login == "coderabbitai[bot]"`. Also pull review-level comments. Resolved threads are skipped.

5. **Round-based fix loop, max 3 rounds.** For each round:
   - If fix queue (regressions + CodeRabbit comments) is empty → break, proceed to merge.
   - Dispatch a sub-agent with: the regression list, the CodeRabbit comments, the relevant file paths, and the constraint to keep changes minimal (only fix what's flagged, don't refactor unrelated code).
   - After the agent finishes: `bash lint` + `bash test`, commit, push.
   - Wait for CI to re-run (poll with the same announce-and-cap procedure from step 2, this round only).
   - Re-evaluate gates. If both clear → break. Otherwise → next round.
   - After round 3 with gates still failing → **give up**. Print remaining blockers and exit without merging.

6. **Clean tree gate.** Same as `/clud-pr`: every modified/untracked file gets a deliberate commit-or-delete decision. No stash-and-merge tricks.

7. **Merge.** `gh pr merge <num> --squash --delete-branch` (or `--merge` if the repo prefers full history — check repo conventions first). Confirm with `gh pr view <num> --json state,mergedAt`.

   **"Already merged" is success.** If `gh pr merge` returns `pull request <num> was already merged`, treat it as success (a maintainer merged it while we were polling). Skip straight to the post-merge confirmation.

## Hard rules

- **Never merge with regressions.** A check passing on main but failing on the PR is always a blocker, even if "it's flaky" — the agent's job is to make it green or surface it.
- **Never merge with unresolved CodeRabbit comments** unless they're explicitly out-of-scope (e.g., flagging a pre-existing issue). The fix-or-acknowledge decision is per-comment.
- **Never bypass branch protection.** No `--admin`. If the PR is blocked by a required reviewer, surface that and stop.
- **Never `--no-verify`** on the fix-loop commits. Hooks exist for a reason.
- **Give up cleanly after 3 rounds.** Print the remaining blockers, the round count consumed, and exit non-zero so the user knows merge didn't happen.

## Failure modes to avoid

- **Poll loop without a cap** — always stop at the announced cap (default 45 min, adjusted per repo); better to warn-and-stop than burn forever.
- **Silent polling** — without an upfront "polling for up to N min" announcement, the user reads the silent wait as a hung shell. Always announce.
- **CodeRabbit-not-installed deadlock — probe before polling.** On a repo without CodeRabbit, the `coderabbit_reviews != 0` clause never satisfies, so the loop spins until the cap fires (issue #194; observed: ~20 min silent stall on `zackees/zccache#523`). Always run the 5-merged-PR `coderabbitai[bot]` review-presence probe in step 2 and drop the CR clause when the sample is zero. Do not "warn and continue" — a category-error gate is not a relaxed gate.
- **Treating timeout as merge-eligible** — no signal ≠ green signal. Refuse to merge in the timeout case.
- **Conflating "passing on main" with "not a regression"** — only the *same check name* on main counts. Renamed/added checks are new regressions until proven otherwise.
- **Treating "check missing from main" as "passing on main"** — PR-only workflows (e.g. `pull_request_target`) never run on `push` events, so they're invisible in main's check-runs. Cross-reference recent PRs before classifying.
- **Trusting `gh pr checks` exit code** — it returns 1 whenever any check failed, even with `--json`. Always read the output and ignore `$?`.
- **Leading `sleep N; <cmd>` chains** — the Claude Code harness blocks these. Use `until <check>; do sleep N; done` (sleep inside the body), or `run_in_background: true` for fire-and-forget waits, or `ScheduleWakeup` for >5min waits.
- **CI poll that doesn't watch PR state** — if `gh pr view <num> --json state` is not checked every iteration, the loop will keep spinning for 20+ minutes after a merge (GitHub Actions jobs do not cancel on merge, so PENDING checks remain PENDING). Always poll PR state first; break on MERGED/CLOSED.
- **Background poll left running after merge confirmation** — once you see `state: MERGED` from any source, kill the background poll task explicitly. Do NOT rely on "it'll self-terminate" — the loop's exit condition is moot but the loop body doesn't know that, and respawned `sleep 30` processes will continue to spam the host until the cap is hit.
- **Letting the fix-agent expand scope** — pass a tight scope; if the agent finds an unrelated bug, it gets noted, not fixed in this PR.

## When NOT to use this

- PR is in DRAFT state (waiting for author signal first)
- PR has merge conflicts (needs human rebase)
- PR is blocked on a human reviewer (no automation can resolve that)
- The repo has no CI configured (nothing to gate on)
