---
name: clud-pr
description: Implement a GitHub issue, PR follow-up, or freeform task inside a .claude/ worktree and ship one clean PR; also follow an open PR through CI/review fixes to merge when asked to land or ship it.
triggers:
  - When the user asks to implement a GitHub issue
  - When the user gives a freeform task and asks to ship it as a PR
  - When the user passes a PR URL/number to "/clud-pr" (triage mode)
  - When the user asks to merge, land, or ship an open PR, including legacy "/clud-pr-merge"
  - When the user says "ship", "do-pr", "/clud-pr", or references an issue URL/number with intent to deliver
---
<!-- managed-by: clud -->

# /clud-pr

Read the user's task, express the bug fix or feature requirement as a failing test first, implement the fix until that test turns green, push it as one PR with no files left behind, then give the user the PR URL. The task may be an issue URL, issue number, PR URL, PR number, or a plain sentence.

Five hard rules:

1. **Lock the goal in.** Before any code or merge work, invoke the `/goal <one-line deliverable>` slash command so the harness Stop hook blocks until you ship. Use the deliverable phrasings under each mode below. Skip only for PR Triage Mode until triage decides to act.
2. **Use a disposable worktree.** Do all PR work inside `.claude/worktrees/<branch>/`, never on the main checkout.
3. **Push the PR.** Finish with `gh pr create`; a local branch or commit is not the deliverable.
4. **Leave nothing behind.** After the PR exists, remove the worktree and verify the main checkout's `git status` is clean.
5. **RED -> GREEN for code changes.** Before implementing a bug fix or feature, add or identify an automated test that fails because the requirement is unmet. Run the focused test and capture the RED failure. Then implement until that test is GREEN. If no automated test is practical, document the reason and use the closest executable repro/check before coding.

## Worktree Workspace

1. **`.gitignore` gate.** Check that `.gitignore` covers `.claude/` (for example `.claude/`, `.claude/**`, or `/.claude/`).
   - If yes, continue.
   - If no, delete any `.claude/worktrees/` created during this run, then either ask to add `.claude/` to `.gitignore` or use a sibling path outside the repo (`../<repo>-wt-<branch>/`). Do not create an unignored worktree inside the repo.
2. **Stale worktrees.** Before creating a new worktree, run `git worktree prune`, then scan `.claude/worktrees/*` for directories older than 24 hours. If any exist, list them and ask whether to delete them. Do not remove stale worktrees without permission.
3. **Create the worktree.** `git fetch origin main && git worktree add -b feat/<short-name> .claude/worktrees/<branch> origin/main`. All edits, commits, lint, and tests happen inside that path.
4. **Tear down.** After `gh pr create` succeeds, audit live processes before removing the worktree. Search for processes whose executable path or command line references `.claude/worktrees/<branch>` (on Windows, `Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -like '*<branch>*' -or $_.CommandLine -like '*<worktree-path>*' }`). If a matching process is useful verification that is still making progress, wait and report status. If it is an abandoned or timed-out child from this run, stop only that exact process tree before cleanup and record what was stopped. Never kill guessed or unrelated processes. Only after the audit is clear, run `git worktree remove .claude/worktrees/<branch>`, then `git worktree prune`, and confirm the entry is gone from `git worktree list`. If removal still fails with a lock, re-audit exact holders or use `clud trash` for a quarantine fallback; do not use a blind `rm -rf` retry loop.

## Mode Selection

Look at the input first:

- **PR URL / number** -> run **PR triage mode**. Do not branch or code until triage decides what to do.
- **Merge / land / ship current PR** -> run **PR merge mode**.
- **Meta tracking issue** (the issue body contains a multi-item checklist of sub-issue references — `- [ ] #NNN` lines, a "Burn-down checklist" section, or a title prefixed `meta:` / `[META]`) -> run **Meta tracking issue mode** before any per-item PR work.
- **Issue URL / number / freeform task sentence** -> run **task to PR workflow**.

## Meta Tracking Issue Mode

Meta tracking issues do NOT close via a PR merge — they close when every sub-item is closed AND no other open issues exist in the repo. Running the normal Task To PR Workflow against a meta issue deadlocks the `/goal` hook: PRs ship for sub-items, but the meta stays open and the hook keeps firing until the harness hits its `CLAUDE_CODE_STOP_HOOK_BLOCK_CAP` (default 100). The fix is to triage what the session can ship, sidecar the rest, and use a goal phrasing whose success criterion is reachable inside one session.

1. **Set the goal with meta phrasing.** Invoke `/goal Triage <meta-issue> burn-down: ship in-scope PRs, open sidecar tracking issue for deferred items, report URLs.` so the Stop hook's success criterion includes the sidecar. Do NOT use the per-issue `Ship PR for <issue>` phrasing — that phrasing's hook can only be satisfied by closing the meta itself, which is structurally out of session scope.

2. **Triage on entry.** Read every unchecked checklist item. Classify each as exactly one of:
   - `in-scope` — concrete code work, single-PR sized, no external blockers, doable this session.
   - `blocked-on-merge:#X` — only actionable after another in-flight PR (`#X`) lands first.
   - `blocked-on-external` — needs cross-repo work, field/observability data, or a third party.
   - `design` — multi-week implementation phase following a design proposal.

   Post the classification as a comment on the meta issue **before** opening any PRs. The comment commits the triage in writing and creates the audit trail.

3. **Sidecar the deferred items.** Choose one:
   - **Sidecar issue (preferred).** `gh issue create --repo <owner>/<repo> --title "meta: deferred from #<original> burn-down (<date>) — out-of-session items"` with the deferred items as its body checklist. Cross-link it in the original meta's body.
   - **In-meta deferred section.** Edit the original meta's body to move deferred items under `## Out of session scope (tracked in <link or note>)`. They keep their visibility but leave the gating checklist.

   Either way, the original meta's gating checklist now contains only `in-scope` items.

4. **Burn down the in-scope set.** For each `in-scope` item, run the Task To PR Workflow per item, opening one PR each. Each PR follows the standard RED -> GREEN, lint, test, push, teardown cycle. The per-item PR's own `/goal` invocation in step 2 of Task To PR Workflow is fine — those goals are reachable.

5. **Resolve the meta-mode session.** When every `in-scope` PR is open and the sidecar exists, comment on the meta summarizing what shipped + linking the sidecar. The session can stop cleanly — the hook's success criterion (in-scope shipped + sidecar opened) is satisfied. The meta itself will close later, by user action, after the PRs land.

6. **Final response.** List the PR URLs shipped, the sidecar issue URL, and a one-line note on what the sidecar tracks.

**Do NOT mid-session move items from `in-scope` to `deferred` to escape the hook.** That corrupts the triage signal and reduces this whole mode to scope fudging. The triage at step 2 commits at entry. If your entry triage was wrong, surface that explicitly in a meta comment and stop — re-triaging is a fresh session's job.

## PR Merge Mode

Use this when the user asks to merge, land, or ship an already-open PR.

1. **Set the goal.** Invoke `/goal Merge PR #<num> to main and report the merged URL.` so the Stop hook blocks until the PR is merged or you explicitly clear the goal.
2. **Resolve the PR.** `gh pr view --json number,headRefName,baseRefName,state,mergeable,statusCheckRollup,url`. Refuse if state is not `OPEN` or mergeability is a clear blocker.
3. **Poll with a cap and visible status.** Estimate recent CI duration from `gh run list --branch <default> --limit 10 --json conclusion,startedAt,updatedAt`; announce the cap before waiting. Poll every 30 seconds, and check `gh pr view <num> --json state` first every time so a manually merged/closed PR exits immediately.
4. **Classify CI failures.** A check passing on default but failing on the PR is a regression. A check already failing on default is pre-existing and not merge-blocking. A PR-only check missing on default must be compared against recent PRs before calling it pre-existing.
5. **Collect review findings.** Pull CodeRabbit review comments and unresolved substantive review comments. Skip praise, nits, resolved/outdated threads, and comments explicitly out of scope.
6. **Fix rounds, max 3.** For each regression or actionable review finding, follow RED -> GREEN: add or identify the focused failing test/repro/CI check first, capture the RED failure, implement only the scoped fix, then verify that focused signal is GREEN before running `bash lint` and `bash test`. Commit and push each completed round.
7. **Merge only when gates are green.** Never merge with PR regressions, unresolved substantive review comments, merge conflicts, required human reviews, or timed-out CI. If `gh pr merge` reports the PR was already merged, treat that as success.
8. **Final response.** Give the merged PR URL and the checks/fix rounds that mattered.

## PR Triage Mode

1. **Verify the PR exists.** `gh pr view <url-or-num> --json number,state,title,url,headRefName,mergedAt,closedAt,author`. If it fails, say so and stop.
2. **Branch on state.**
   - **OPEN** -> report current state and ask what the user wants done.
   - **MERGED** -> say so and stop; follow-up work belongs in a new PR or issue.
   - **CLOSED, not merged** -> continue.
3. **Check CodeRabbit review comments.** Use `gh pr view <num> --json reviews,comments` and `gh api repos/<owner>/<repo>/pulls/<num>/comments`. Count only actionable comments from `coderabbitai*`; skip praise and nit-only threads.
4. **Check failing CI.** Use `gh pr checks <num>` or recent `gh run list --branch <headRefName> --limit 5`.
5. **Check for an existing follow-up PR.** Search PRs and the closed PR timeline for later work that appears to address the comments or CI.
6. **Ask before acting.** For each real finding, ask whether to address it in a new PR. If the user says yes, continue through the task to PR workflow and reference the closed PR in the new PR body.

## Task To PR Workflow

1. **Read the task.** If the input includes an issue URL/number, fetch it with `gh issue view <num>` or the URL and extract acceptance criteria. If it is a plain sentence, treat that sentence as the acceptance criteria and infer the repo from the current checkout.
2. **Set the goal.** After reading the task and before any worktree or code work, invoke `/goal Ship PR for <issue-or-task-summary>: report URL and confirm main checkout is clean.` so the Stop hook blocks until the PR URL is reported and the worktree is torn down. If issue-only checks resolve the work without a new PR, clear the goal before stopping.
3. **Issue-only checks.** For issue inputs only:
   - Verify the issue is open.
   - Search for PRs that already resolve it.
   - If a resolving PR is already merged to the default branch, offer to close the issue and stop.
   - If the issue is blocked by same-repo dependencies, take those first in dependency order unless the user overrides. Cross-repo dependencies require explicit user approval per repo.
4. **Set up the worktree.** Follow the Worktree Workspace section.
5. **Plan the fix.** Identify touched files and decide whether independent chunks can run in parallel. Use parallel agents only for genuinely independent file-scoped work; otherwise implement directly.
6. **Prove RED.** For a bug, add a regression test that fails on the current behavior. For a feature, add or update the narrowest unit/integration/e2e test that encodes the acceptance criteria and fails before the implementation. Run the focused test before coding and keep the failure output for the PR/testing note.
7. **Implement to GREEN.** Give any agent its exact scope, owned files, worktree path, the RED test command/output, and instruction not to touch files outside scope. Implement only enough to satisfy the requirement, then re-run the focused test until it passes.
8. **Lint and test.** Run the repo's normal lint and test commands inside the worktree after the focused RED test is GREEN. Do not use `--no-verify`; fix hook failures.
9. **Clean tree gate.** Run `git status` inside the worktree. Commit source changes that belong in the PR. Delete tmp, scratch, and build artifacts. The worktree must be clean before push.
10. **Commit.** Use a conventional commit. Reference the issue when one exists (`Closes #<num>` in the commit body or PR body). For freeform tasks, summarize the task instead.
11. **Push and open PR.** `git push -u origin <branch>`, then `gh pr create`. The body should include the issue link when present, a concise summary, and tests run.
12. **Remove the worktree.** Follow the **Tear down** process-audit pattern in the Worktree Workspace section: wait for useful matching subprocesses, stop only exact abandoned/timed-out matching process trees, then run `git worktree remove`, `git worktree prune`, confirm the entry is gone, and verify the main checkout's `git status` is clean.
13. **Final response.** Give the PR URL and any essential test note. Keep it short.

## Failure Modes To Avoid

- Starting code work for a PR link before triage.
- Treating a freeform sentence as if it must have an issue number.
- Implementing a closed issue or an issue already resolved on the default branch.
- Calling an issue resolved when its PR merged only into a stack branch.
- Creating multiple PRs for one requested task.
- Implementing before proving RED, or claiming coverage from a test that never failed for the bug/requirement.
- Pushing with a dirty worktree or leaving `.claude/worktrees/<branch>/` behind.
- Skipping lint, tests, or failing hooks.
- Auto-deleting stale worktrees without asking.
- Running Task To PR Workflow on a meta tracking issue (multi-item checklist input) without going through **Meta Tracking Issue Mode** first. The `/goal` hook deadlocks because the meta does not close on a single PR merge.
- Using the per-issue `/goal Ship PR for <issue>` phrasing for a meta input. Meta inputs use the meta phrasing in step 1 of Meta Tracking Issue Mode so the hook's success criterion includes the sidecar.
- Mid-session re-classifying a meta sub-item from `in-scope` to `deferred` to escape a blocking `/goal` hook. Triage commits at entry; if it was wrong, surface that explicitly and stop.

## When Not To Use This

- The user only wants investigation, a plan, or a review.
- The work needs design discussion before implementation.
- A PR link triage shows no actionable findings.
