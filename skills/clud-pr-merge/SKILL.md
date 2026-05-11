<!-- managed-by: clud -->
---
name: clud-pr-merge
description: Wait for CI + CodeRabbit on the current PR, fix regressions and review comments, then merge to main.
triggers:
  - When the user asks to merge a PR
  - When the user says "/clud-pr-merge", "ship it", "land this PR"
  - After /clud-pr completes and the user wants to follow through to merge
---

# /clud-pr-merge

Take an open PR from "tests running" to "merged into main". Two gates:

1. **CI gate** — all required checks green, OR every failing check was already failing on main (no regressions introduced by this PR).
2. **Review gate** — no unresolved CodeRabbit comments.

If the gates fail in fixable ways, dispatch a sub-agent to fix; up to 3 rounds total. After 3, give up and surface the blockers.

## Workflow

1. **Resolve the PR.** `gh pr view --json number,headRefName,baseRefName,state,mergeable,statusCheckRollup,url` against the current branch. Refuse if state ≠ OPEN or mergeable ≠ MERGEABLE/UNKNOWN.

2. **Poll until CI + CodeRabbit have reported.** Loop with **30-second sleeps**, capped at **12 minutes total** (24 iterations). Each iteration fetches:
   - `gh pr checks <num> --json name,state,bucket` — every required check has a non-PENDING state.
   - `gh api repos/{owner}/{repo}/pulls/{num}/reviews --jq '.[] | select(.user.login=="coderabbitai")'` — at least one CodeRabbit review (or comment) has been posted.

   Both must report at least once. If 12 minutes pass with neither reporting, **give up and warn the user**: `[clud-pr-merge] timed out waiting for CI/CodeRabbit after 12 minutes. Re-run when checks have reported.` Do NOT merge.

3. **Diff CI failures vs main.** For every failing check on the PR:
   - Fetch the same check name on `main`'s latest commit: `gh api repos/{owner}/{repo}/commits/main/check-runs`.
   - If the same check is also failing on main → **pre-existing failure**, not a regression. Mark and skip.
   - If the check is passing on main but failing on the PR → **regression**. Add to the fix queue.

4. **Collect CodeRabbit findings.** `gh api repos/{owner}/{repo}/pulls/{num}/comments` filtered to `user.login == "coderabbitai"`. Also pull review-level comments. Resolved threads are skipped.

5. **Round-based fix loop, max 3 rounds.** For each round:
   - If fix queue (regressions + CodeRabbit comments) is empty → break, proceed to merge.
   - Dispatch a sub-agent with: the regression list, the CodeRabbit comments, the relevant file paths, and the constraint to keep changes minimal (only fix what's flagged, don't refactor unrelated code).
   - After the agent finishes: `bash lint` + `bash test`, commit, push.
   - Wait for CI to re-run (poll with the 30s/12min budget again, this round only).
   - Re-evaluate gates. If both clear → break. Otherwise → next round.
   - After round 3 with gates still failing → **give up**. Print remaining blockers and exit without merging.

6. **Clean tree gate.** Same as `/clud-pr`: every modified/untracked file gets a deliberate commit-or-delete decision. No stash-and-merge tricks.

7. **Merge.** `gh pr merge <num> --squash --delete-branch` (or `--merge` if the repo prefers full history — check repo conventions first). Confirm with `gh pr view <num> --json state,mergedAt`.

## Hard rules

- **Never merge with regressions.** A check passing on main but failing on the PR is always a blocker, even if "it's flaky" — the agent's job is to make it green or surface it.
- **Never merge with unresolved CodeRabbit comments** unless they're explicitly out-of-scope (e.g., flagging a pre-existing issue). The fix-or-acknowledge decision is per-comment.
- **Never bypass branch protection.** No `--admin`. If the PR is blocked by a required reviewer, surface that and stop.
- **Never `--no-verify`** on the fix-loop commits. Hooks exist for a reason.
- **Give up cleanly after 3 rounds.** Print the remaining blockers, the round count consumed, and exit non-zero so the user knows merge didn't happen.

## Failure modes to avoid

- **Poll loop without a cap** — always stop at 12 minutes; better to warn-and-stop than burn forever.
- **Treating timeout as merge-eligible** — no signal ≠ green signal. Refuse to merge in the timeout case.
- **Conflating "passing on main" with "not a regression"** — only the *same check name* on main counts. Renamed/added checks are new regressions until proven otherwise.
- **Letting the fix-agent expand scope** — pass a tight scope; if the agent finds an unrelated bug, it gets noted, not fixed in this PR.

## When NOT to use this

- PR is in DRAFT state (waiting for author signal first)
- PR has merge conflicts (needs human rebase)
- PR is blocked on a human reviewer (no automation can resolve that)
- The repo has no CI configured (nothing to gate on)
