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

## Code Change Rule

When the fix-loop modifies code to clear a regression or address a CodeRabbit comment, work the change RED -> GREEN: pin the failure with a focused test or repro first, make the smallest change that flips the signal, then run lint + the same focused test to confirm the green before pushing.

## Workflow

1. **Resolve the PR.** `gh pr view --json number,headRefName,baseRefName,state,mergeable,statusCheckRollup,url` against the current branch. Refuse if state ≠ OPEN or mergeable ≠ MERGEABLE/UNKNOWN.

2. **Estimate the wait, announce it, then delegate the poll to `pr_merge_watch`.** The whole inline poll loop — PR-state probe, checks probe, CodeRabbit-review probe, cap, cancel-on-fail bookkeeping, all the early-exit edge cases — is encapsulated in the bundled `pr_merge_watch.py` tool. Don't reimplement it inline; call the tool.

   - Probe the repo's *recent* CI duration: `gh run list --repo <owner/repo> --branch main --limit 10 --json conclusion,startedAt,updatedAt`. Take the p90 wall-clock; round up to the nearest 5 min. This is your `--timeout` budget for the tool.
   - **Default cap: 45 minutes** (the FastLED repo and similar embedded projects routinely hit 25–30 min platform builds). Tighten the cap to `max(15, p90 + 5min)` for repos with faster CI; never go below 15 minutes.
   - **Announce upfront** so the user knows what to expect; silent polling reads as a hung shell.
     - `[clud-pr-merge] polling CI for up to <N> min via pr_merge_watch; slowest recent build was <P90> min.`
   - Then run the tool. Pass `--timeout` as `<N>*60` seconds; everything else defaults are fine for the standard case:
     ```
     clud tool run github/pr_merge_watch.py <PR#> --timeout <N*60>
     ```
     The tool internally polls `gh pr view`, `gh pr checks`, and (when CodeRabbit has reviewed prior merged PRs in this repo) the `coderabbitai[bot]` review stream — handling all the gotchas listed below for you. Inspect the exit code:
     - **`0`** — all required checks green AND PR mergeable. Proceed to step 6 (clean tree gate) then step 7 (merge).
     - **`1`** — at least one required check failed. Tool prints the failing check name + first error + a classifier label (e.g. "rustfmt drift", "clippy warning", "test failure") on stdout. Proceed to step 3 (regression diff) then step 5 (fix loop).
     - **`2`** — new CodeRabbit / human review activity. Proceed to step 4 (collect comments) then step 5 (fix loop).
     - **`3`** — PR was merged or closed out from under us. If MERGED, success (skip to step 7's post-merge confirmation). If CLOSED, abort with a user-facing message.
     - **`4`** — timeout. Give up: `[clud-pr-merge] timed out waiting for CI/CodeRabbit after <N> minutes. Re-run when checks have reported.` Do NOT merge.

   **Why delegate.** The tool encapsulates several edge cases that every inline poll re-discovers the hard way:
   - **PR-state early-exit.** The tool checks `pr view --json state` first each iteration and exits 3 the moment state ≠ OPEN. Without that, GitHub Actions jobs that don't cancel on merge keep "PENDING" forever and the poll hangs (issue: `zackees/zccache` ~20-min stalls on merged PRs).
   - **CodeRabbit-not-installed graceful degradation.** The tool never blocks on CR reviews if the repo doesn't have CR configured — it just exits when checks settle. No need for the 5-merged-PR CR-presence probe.
   - **`gh pr checks` exit-code gotcha.** `gh pr checks` returns non-zero whenever any check failed even with `--json`. The tool reads stdout regardless of `$?`; an inline poll has to remember this.
   - **Cancel-on-exit.** On a non-success exit the tool cancels still-running workflow runs on the PR's head SHA so we stop burning matrix minutes on results we've already decided to ignore.

   **If you need to background the wait** (so the agent does other work in parallel), pass it through `run_in_background: true` and treat the task-notification's exit code the same way as a foreground invocation. The tool handles its own polling cadence (default 60 s) so no `until`/`sleep` wrapping is needed.

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
   - Wait for CI to re-run using the same `clud tool run github/pr_merge_watch.py <PR#> --timeout <N*60>` call as step 2. Interpret the exit code the same way.
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

The tool (`pr_merge_watch.py`) handles the polling-specific failure modes — PR-state early-exit, CodeRabbit-absent deadlock, `gh pr checks` exit-code quirk, cancel-on-exit, cap enforcement. They're listed in the tool's docstring + repo issues #194/#195. The failure modes that remain the *skill's* responsibility:

- **Silent polling** — even with the tool delegating the wait, the skill must emit the upfront `[clud-pr-merge] polling CI for up to <N> min ...` announcement so a foreground invocation doesn't look hung.
- **Treating timeout as merge-eligible** — exit code 4 from the tool means "no signal arrived in time," not "green." Refuse to merge in the timeout case.
- **Reimplementing the poll inline** — don't write your own `until ... sleep` loop with `gh pr view`/`gh pr checks`/`gh api` calls. The tool exists exactly because every inline attempt rediscovers the same edge cases. If the tool's behavior is wrong for your case, fix the tool, don't fork it.
- **Conflating "passing on main" with "not a regression"** — only the *same check name* on main counts. Renamed/added checks are new regressions until proven otherwise.
- **Treating "check missing from main" as "passing on main"** — PR-only workflows (e.g. `pull_request_target`) never run on `push` events, so they're invisible in main's check-runs. Cross-reference recent PRs before classifying.
- **Letting the fix-agent expand scope** — pass a tight scope; if the agent finds an unrelated bug, it gets noted, not fixed in this PR.

## When NOT to use this

- PR is in DRAFT state (waiting for author signal first)
- PR has merge conflicts (needs human rebase)
- PR is blocked on a human reviewer (no automation can resolve that)
- The repo has no CI configured (nothing to gate on)
