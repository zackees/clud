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

2. **Estimate the wait, announce it, then poll.** Before sleeping:
   - Probe the repo's *recent* CI duration: `gh run list --repo <owner/repo> --branch main --limit 10 --json conclusion,startedAt,updatedAt`. Take the p90 wall-clock; round up to the nearest 5 min. This is your expected wait.
   - **Default cap: 45 minutes** (the FastLED repo and similar embedded projects routinely hit 25–30 min platform builds). Tighten the cap to `max(15, p90 + 5min)` for repos with faster CI; never go below 15 minutes.
   - **Announce upfront** with one user-facing line: `[clud-pr-merge] polling CI for up to <N> min; slowest recent build was <P90> min.` Silent polling reads as a hung shell — never skip this announcement.
   - Then loop with **30-second sleeps**. Each iteration fetches:
     - `gh pr checks <num> --json name,state,bucket` — every required check has a non-PENDING state.
     - `gh api repos/{owner}/{repo}/pulls/{num}/reviews --jq '.[] | select(.user.login=="coderabbitai[bot]")'` — at least one CodeRabbit review has been posted (or CodeRabbit explicitly "skipped" the review, which counts as reported).

   Both must report at least once. If the cap elapses with neither reporting, **give up and warn the user**: `[clud-pr-merge] timed out waiting for CI/CodeRabbit after <N> minutes. Re-run when checks have reported.` Do NOT merge.

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
- **Treating timeout as merge-eligible** — no signal ≠ green signal. Refuse to merge in the timeout case.
- **Conflating "passing on main" with "not a regression"** — only the *same check name* on main counts. Renamed/added checks are new regressions until proven otherwise.
- **Treating "check missing from main" as "passing on main"** — PR-only workflows (e.g. `pull_request_target`) never run on `push` events, so they're invisible in main's check-runs. Cross-reference recent PRs before classifying.
- **Trusting `gh pr checks` exit code** — it returns 1 whenever any check failed, even with `--json`. Always read the output and ignore `$?`.
- **Leading `sleep N; <cmd>` chains** — the Claude Code harness blocks these. Use `until <check>; do sleep N; done` (sleep inside the body), or `run_in_background: true` for fire-and-forget waits, or `ScheduleWakeup` for >5min waits.
- **Letting the fix-agent expand scope** — pass a tight scope; if the agent finds an unrelated bug, it gets noted, not fixed in this PR.

## When NOT to use this

- PR is in DRAFT state (waiting for author signal first)
- PR has merge conflicts (needs human rebase)
- PR is blocked on a human reviewer (no automation can resolve that)
- The repo has no CI configured (nothing to gate on)
