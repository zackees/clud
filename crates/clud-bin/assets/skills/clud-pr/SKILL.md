---
name: clud-pr
description: Implement a GitHub issue, PR follow-up, or freeform task inside a .claude/ worktree and ship one clean PR.
triggers:
  - When the user asks to implement a GitHub issue
  - When the user gives a freeform task and asks to ship it as a PR
  - When the user passes a PR URL/number to "/clud-pr" (triage mode)
  - When the user says "ship", "do-pr", "/clud-pr", or references an issue URL/number with intent to deliver
---
<!-- managed-by: clud -->

# /clud-pr

Read the user's task, plan the fix, do it, push it as one PR with no files left behind, then give the user the PR URL. The task may be an issue URL, issue number, PR URL, PR number, or a plain sentence.

Three hard rules:

1. **Use a disposable worktree.** Do all PR work inside `.claude/worktrees/<branch>/`, never on the main checkout.
2. **Push the PR.** Finish with `gh pr create`; a local branch or commit is not the deliverable.
3. **Leave nothing behind.** After the PR exists, remove the worktree and verify the main checkout's `git status` is clean.

## Worktree Workspace

1. **`.gitignore` gate.** Check that `.gitignore` covers `.claude/` (for example `.claude/`, `.claude/**`, or `/.claude/`).
   - If yes, continue.
   - If no, delete any `.claude/worktrees/` created during this run, then either ask to add `.claude/` to `.gitignore` or use a sibling path outside the repo (`../<repo>-wt-<branch>/`). Do not create an unignored worktree inside the repo.
2. **Stale worktrees.** Before creating a new worktree, run `git worktree prune`, then scan `.claude/worktrees/*` for directories older than 24 hours. If any exist, list them and ask whether to delete them. Do not remove stale worktrees without permission.
3. **Create the worktree.** `git fetch origin main && git worktree add -b feat/<short-name> .claude/worktrees/<branch> origin/main`. All edits, commits, lint, and tests happen inside that path.
4. **Tear down.** After `gh pr create` succeeds, run `git worktree remove .claude/worktrees/<branch>`, then `git worktree prune`, and confirm the entry is gone from `git worktree list`.

## Mode Selection

Look at the input first:

- **PR URL / number** -> run **PR triage mode**. Do not branch or code until triage decides what to do.
- **Issue URL / number / freeform task sentence** -> run **task to PR workflow**.

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
2. **Issue-only checks.** For issue inputs only:
   - Verify the issue is open.
   - Search for PRs that already resolve it.
   - If a resolving PR is already merged to the default branch, offer to close the issue and stop.
   - If the issue is blocked by same-repo dependencies, take those first in dependency order unless the user overrides. Cross-repo dependencies require explicit user approval per repo.
3. **Set up the worktree.** Follow the Worktree Workspace section.
4. **Plan the fix.** Identify touched files and decide whether independent chunks can run in parallel. Use parallel agents only for genuinely independent file-scoped work; otherwise implement directly.
5. **Implement.** Give any agent its exact scope, owned files, worktree path, and instruction not to touch files outside scope.
6. **Lint and test.** Run the repo's normal lint and test commands inside the worktree. Do not use `--no-verify`; fix hook failures.
7. **Clean tree gate.** Run `git status` inside the worktree. Commit source changes that belong in the PR. Delete tmp, scratch, and build artifacts. The worktree must be clean before push.
8. **Commit.** Use a conventional commit. Reference the issue when one exists (`Closes #<num>` in the commit body or PR body). For freeform tasks, summarize the task instead.
9. **Push and open PR.** `git push -u origin <branch>`, then `gh pr create`. The body should include the issue link when present, a concise summary, and tests run.
10. **Remove the worktree.** Run `git worktree remove .claude/worktrees/<branch>`, then `git worktree prune`, confirm it is gone, then verify the main checkout's `git status` is clean.
11. **Final response.** Give the PR URL and any essential test note. Keep it short.

## Failure Modes To Avoid

- Starting code work for a PR link before triage.
- Treating a freeform sentence as if it must have an issue number.
- Implementing a closed issue or an issue already resolved on the default branch.
- Calling an issue resolved when its PR merged only into a stack branch.
- Creating multiple PRs for one requested task.
- Pushing with a dirty worktree or leaving `.claude/worktrees/<branch>/` behind.
- Skipping lint, tests, or failing hooks.
- Auto-deleting stale worktrees without asking.

## When Not To Use This

- The user only wants investigation, a plan, or a review.
- The work needs design discussion before implementation.
- A PR link triage shows no actionable findings.
