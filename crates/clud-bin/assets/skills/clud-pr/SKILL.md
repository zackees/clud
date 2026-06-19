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

Six hard rules:

1. **Lock the goal in, unless delegated.** Before any code or merge work, invoke the `/goal <one-line deliverable>` slash command so the harness Stop hook blocks until you ship. Use the deliverable phrasings under each mode below. Skip only for PR Triage Mode until triage decides to act, or when the caller explicitly says this is delegated from [[clud-fix]] and an outer issue-level goal is already active.
2. **Use a disposable worktree.** Do all PR work inside `.claude/worktrees/<branch>/`, never on the main checkout.
3. **Push the PR.** Finish with `gh pr create`; a local branch or commit is not the deliverable.
4. **Leave nothing behind.** After the PR exists, remove the worktree and verify the main checkout's `git status` is clean.
5. **RED -> GREEN for code changes.** Before implementing a bug fix or feature, add or identify an automated test that fails because the requirement is unmet. Run the focused test and capture the RED failure. Then implement until that test is GREEN. If no automated test is practical, document the reason and use the closest executable repro/check before coding.
6. **Delegated mode returns evidence.** When called by [[clud-fix]], do not set or replace `/goal`; return structured evidence for the outer orchestrator instead.

## Worktree Workspace

1. **`.gitignore` gate.** Check that `.gitignore` covers `.claude/` (for example `.claude/`, `.claude/**`, or `/.claude/`).
   - If yes, continue.
   - If no, delete any `.claude/worktrees/` created during this run, then either ask to add `.claude/` to `.gitignore` or use a sibling path outside the repo (`../<repo>-wt-<branch>/`). Do not create an unignored worktree inside the repo.
2. **Stale worktrees.** Before creating a new worktree, run `git worktree prune`, then scan `.claude/worktrees/*` for directories older than 24 hours. If any exist, list them and ask whether to delete them. Do not remove stale worktrees without permission.
3. **Create the worktree.** `git fetch origin main && git worktree add -b feat/<short-name> .claude/worktrees/<branch> origin/main`. All edits, commits, lint, and tests happen inside that path.
4. **Tear down.** After `gh pr create` succeeds, audit live processes before removing the worktree. Search for processes whose executable path or command line references `.claude/worktrees/<branch>` (on Windows, `Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -like '*<branch>*' -or $_.CommandLine -like '*<worktree-path>*' }`). If a matching process is useful verification that is still making progress, wait and report status. If it is an abandoned or timed-out child from this run, stop only that exact process tree before cleanup and record what was stopped. Never kill guessed or unrelated processes. Only after the audit is clear, run `git worktree remove .claude/worktrees/<branch>`, then `git worktree prune`, and confirm the entry is gone from `git worktree list`. If removal still fails with a lock, re-audit exact holders or use `clud trash` for a quarantine fallback; do not use a blind `rm -rf` retry loop.

## Input Recognition

Before mode selection, classify the input to distinguish PR URLs, issue URLs, and bare numbers. GitHub shares the integer namespace between issues and PRs in a single repo — a number is one or the other but never both — so the bare-number probe is unambiguous once both APIs are checked.

| Input shape | Detection rule | Resolves to |
|---|---|---|
| `https://github.com/<o>/<r>/pull/<N>` (singular `pull`) | URL path contains `/pull/` | PR |
| `https://github.com/<o>/<r>/pulls/<N>` (plural) | `gh` normalizes this | PR |
| `https://github.com/<o>/<r>/issues/<N>` | URL path contains `/issues/` | issue |
| Bare `#<N>` or `<N>` | try `gh pr view <N> --repo <o>/<r> --json number,state` first — on success treat as PR; else `gh issue view <N>` and treat as issue | PR or issue |
| `pr:<N>` or `issue:<N>` prefix in the invocation | explicit override; skip probing | per prefix |
| Freeform task sentence | not a reference; no probing | freeform task |

If the input is a bare number and both `gh pr view` and `gh issue view` succeed, that's a contradiction of GitHub's namespace rule — refuse with `unknown-reference` and stop. If neither succeeds, refuse with `unknown-reference` and stop.

All `gh` examples below are GitHub-specific. The skill supports other forges via the `## Forge support` section below — substitute the matching native CLI per forge.

## Forge support

This skill defaults to GitHub and the `gh` CLI for backwards compatibility. URL inputs from other forges are classified by URL prefix and routed to the matching native CLI. Bare numbers (`#<N>`) without an explicit prefix resolve their forge from the current worktree's `git remote get-url origin`.

### Multi-forge URL recognition

| Forge | URL prefix(es) | Native CLI | Vocabulary |
|---|---|---|---|
| GitHub | `github.com/<o>/<r>/(issues\|pull)/<N>` | `gh` | issue / PR (`#N`) |
| GitLab | `gitlab.com/<g>/<p>/-/(issues\|merge_requests)/<N>` and self-hosted variants | `glab` | issue / **merge request (MR)** (`!N`) |
| Bitbucket | `bitbucket.org/<o>/<r>/(issues\|pull-requests)/<N>` | none official; REST API | issue / PR (`#N`) |
| Gitea | `<host>/<o>/<r>/(issues\|pulls)/<N>` | `tea` | issue / PR (`#N`) |
| Forgejo | `<host>/<o>/<r>/(issues\|pulls)/<N>` (same patterns as Gitea) | `forgejo-cli` (early) or `tea` | issue / PR (`#N`) |
| Self-hosted GitLab / Gitea / Forgejo | same patterns under custom domains | same CLI | same vocabulary |

The classifier returns `{forge, kind, owner, repo, number, host}` for any URL input.

### Bare-number resolution

When the input is a bare `#<N>` or `<N>`:

1. Run `git remote get-url origin` in the current worktree.
2. Match the remote URL against the forge patterns above.
3. Use the resolved forge for the bare-number probe (`gh pr view` for GitHub, `glab mr view` for GitLab, etc.).

### Explicit prefix override

Prefixes in the invocation force a specific forge and skip remote inference: `github:<N>` / `gitlab:<N>` / `bitbucket:<N>` / `gitea:<N>` / `forgejo:<N>`.

### CLI abstraction

All `gh` examples elsewhere in this skill are GitHub-specific. Substitute the matching native CLI per forge:

- `gh issue view <N>` ↔ `glab issue view <N>` ↔ `tea issues show <N>` ↔ Bitbucket REST: `curl ... /repositories/<o>/<r>/issues/<N>`
- `gh pr view <N>` ↔ `glab mr view <N>` ↔ `tea pulls show <N>` ↔ Bitbucket REST: `curl ... /pullrequests/<N>`
- `gh pr merge <N> --squash` ↔ `glab mr merge <N> --squash` ↔ `tea pulls merge <N>` ↔ Bitbucket REST: `PUT /pullrequests/<N>/merge`

### Vocabulary translation

Internal skill logic can keep saying "PR" generically. User-facing output uses the forge's native vocabulary:

- GitHub user sees `PR #123 merged` — unchanged.
- GitLab user sees `MR !123 merged` (note the `!` sigil GitLab uses instead of `#` for MR references).
- Bitbucket / Gitea / Forgejo users see `PR #123 merged`.

Never silently translate vocabulary in error messages — if a GitLab MR is mentioned, the message says `MR !123`, not `PR #123`.

### Auth-token discovery

Each forge has its own auth model:

- **GitHub**: `gh auth status` or `GITHUB_TOKEN` env var (default).
- **GitLab**: `glab auth status` or `GITLAB_TOKEN` / `GL_TOKEN`.
- **Bitbucket**: App password or workspace token (e.g. `BITBUCKET_TOKEN`).
- **Gitea / Forgejo**: per-host token (`GITEA_TOKEN`, `FORGEJO_TOKEN`).

If the required CLI or token is missing, emit a clear refusal and stop:

```
forge-cli-missing: install <cli> to use clud against <forge>
forge-auth-missing: authenticate to <forge> via <cli> auth login
```

Don't log or persist tokens; rely on the user's existing auth.

### Hard rules

1. **No bundled CLIs.** Discover whether `gh` / `glab` / `tea` / etc. is on PATH; refuse if not. Don't bundle tooling.
2. **GitHub stays the path of least resistance.** Users on GitHub see no behavior change. The forge classifier only kicks in when the URL matches a non-GitHub pattern (or the user passes an explicit non-GitHub prefix).
3. **No silent vocabulary translation in error messages.** If a GitLab MR is mentioned, the message says `MR !123`, not `PR #123`.
4. **No cross-forge operations.** Never move an issue between forges, link a PR to an MR, etc. Single forge per invocation.

## Mode Selection

After Input Recognition, route by the resolved input shape and PR state:

- **Explicitly delegated from [[clud-fix]]** -> run the matching task or merge mode in **Delegated Mode**. Do not set a nested `/goal`.
- **PR URL / number, OPEN** -> run **PR Drive Mode**. Actively drive the PR to a clean mergeable state without asking the user step by step.
- **PR URL / number, CLOSED-not-merged** -> run **PR Triage Mode**. (The user typed a closed PR; triage is the right asking-first scaffold.)
- **PR URL / number, MERGED** -> report and stop; follow-up work belongs in a new PR or issue.
- **Merge / land / ship current PR** -> run **PR Merge Mode**.
- **Issue URL / number / freeform task sentence** -> run **Task To PR Workflow**.

## Delegated Mode

Use this mode when [[clud-fix]] hands off one issue or one PR while it owns an
outer issue-level `/goal`.

1. **Do not invoke `/goal`.** The active goal belongs to [[clud-fix]] and covers
   issue closure, validation, parent checklist updates, and parent closure.
2. **Run the normal task or merge workflow.** All worktree, RED -> GREEN, CI,
   CodeRabbit, lint/test, commit, push, merge, and teardown rules still apply.
3. **Return structured evidence instead of a goal result.** Include:
   - issue URL or PR URL received
   - PR URL opened or merged
   - PR state and merge commit when applicable
   - focused RED/GREEN command and broad checks run
   - remaining blockers, if any
   - issue closure state if known
4. **Do not close parent/meta issues.** Parent issue state is owned by [[clud-fix]].

## PR Merge Mode

Use this when the user asks to merge, land, or ship an already-open PR.

1. **Set the goal unless delegated.** Invoke `/goal Merge PR #<num> to main and report the merged URL.` so the Stop hook blocks until the PR is merged or you explicitly clear the goal. If delegated from [[clud-fix]], skip this nested goal and return structured merge evidence to the caller.
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

## PR Drive Mode

Invoked when Mode Selection resolves to **PR URL / number, OPEN**. The intent is "this PR is open, drive it to a clean mergeable state — check CI, address review comments, resolve conflicts, push the fixes." No step-by-step "ask the user" prompts like PR Triage Mode; act on each finding, with the hard rules below preventing the agent from doing anything unsafe.

### Workflow

1. **Check CI builders.** Run `gh pr checks <pr> --json bucket,name,workflow,link,state`. For each failing or in-progress check:
   - Pull the failure logs (`gh run view <run-id> --log-failed`).
   - Classify the failure: real test failure / lint failure / build failure / known-flake / infrastructure / config error.
   - For local-fixable failures (test, lint, build, config), apply scoped fixes inside a worktree of the PR's head ref.
   - For non-local failures (infra outage, secret/permission missing, external service down), refuse with a structured "needs human" message and stop — do not loop trying random things.

2. **Check code-review comments (AI + human).**
   - `gh pr view <pr> --json reviews,comments`
   - `gh api repos/<owner>/<repo>/pulls/<pr>/comments` (review-thread comments)
   - Filter substantively: include CodeRabbit (`coderabbitai*`), GitHub Copilot review (`github-actions[bot]` for Copilot review identity, `copilot-*`), and any human reviewer with non-nit content. Skip praise, nits, "consider", and threads marked resolved/outdated.
   - Address each actionable comment as a separate commit (or tightly-related batch).

3. **Check files needing resolve.** Run `gh pr view <pr> --json mergeable,mergeStateStatus`:
   - If `mergeable: CONFLICTING` or `mergeStateStatus: DIRTY`, fetch the head ref into a worktree, run `git merge origin/<base>`, resolve the conflicts using the parent skill's existing language-aware conventions where possible (and the per-bucket review rules from `/clud-review` discovery if available).
   - If the conflict is non-trivial (logic-level, not insert-line adjacency like skill-registry inserts), refuse with a structured "needs human merge decision" message and stop.

4. **Apply fixes.** Same worktree-based pattern as `Task To PR Workflow`: small commits per fix, conventional commit messages, no force-push to main, no `--no-verify`, no skipped hooks. When `/clud-review` is wired in as a pre-push step in a future iteration, invoke it on the new commits before push.

5. **Push the new commits.** `git push origin <head-ref>` — regular push, NOT force. The existing commits stay; new commits stack on top. CI auto-re-triggers.

6. **Loop until green or capped.** Reuse the existing PR merge-mode loop caps: at most 3 round-trips through CI / review / conflict surfaces per PR. If still not green after that, surface the blocking finding and stop.

7. **Merge when clean.** Once all three remediation surfaces are clean AND CI is green AND `mergeable: MERGEABLE`, squash-merge and delete the branch. Validate against the underlying issue when one is referenced via `Closes #<N>` (re-run any focused issue-test if `/clud-review`'s issue-test discovery found one).

### Hard rules

1. **No force-push.** Every fix is a new commit on the existing branch. The PR's commit history grows; it never gets rewritten.
2. **No `--no-verify`, no skipped hooks.** Fix the hook failure instead.
3. **One scoped fix per commit.** Multiple unrelated comments → multiple commits. Cosmetic batching is fine (e.g., one commit for "rename `x` → `y` across 4 files") but don't squash a CI fix and a CodeRabbit fix together.
4. **Refuse on uncovered failure shapes.** If the agent can't fix the failure (infrastructure, permissions, logic-level merge conflicts that need a design call), emit a structured "needs-human" terminal and stop. Do NOT enter a retry loop on things the agent doesn't know how to fix.
5. **Cap on the existing PR merge-mode loop caps.** No new outer cap; reuse 3 merge retries / 2 validation retries / 10 PRs total.
6. **Never approve the PR.** The agent applies fixes and pushes; the final merge action is allowed but the agent never `gh pr review --approve` on its own work.

## Task To PR Workflow

1. **Read the task.** If the input includes an issue URL/number, fetch it with `gh issue view <num>` or the URL and extract acceptance criteria. If it is a plain sentence, treat that sentence as the acceptance criteria and infer the repo from the current checkout.
2. **Set the goal unless delegated.** After reading the task and before any worktree or code work, invoke `/goal Ship PR for <issue-or-task-summary>: report URL and confirm main checkout is clean.` so the Stop hook blocks until the PR URL is reported and the worktree is torn down. If delegated from [[clud-fix]], skip this nested goal and return structured PR evidence to the caller. If issue-only checks resolve the work without a new PR, clear the goal before stopping.
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
- Replacing an outer [[clud-fix]] `/goal` with a narrower PR-level goal while in delegated mode.
- Pushing with a dirty worktree or leaving `.claude/worktrees/<branch>/` behind.
- Skipping lint, tests, or failing hooks.
- Auto-deleting stale worktrees without asking.
- **PR Drive Mode: force-pushing the PR branch.** Every fix is a new commit on the existing branch. The PR's commit history grows; it never gets rewritten.
- **PR Drive Mode: `--no-verify` or skipped hooks to get past a failing pre-commit.** Fix the hook failure instead.
- **PR Drive Mode: squashing CI fixes with CodeRabbit-comment fixes into one commit.** One scoped fix per commit so reviewers can read the history.
- **PR Drive Mode: retry-loop on non-fixable failures.** Refuse-and-stop terminals for infra outages, secret/permission missing, logic-level merge conflicts that need a design call. No infinite retries on things the agent doesn't know how to fix.
- **PR Drive Mode: agent self-approval.** The agent applies fixes and pushes; the final merge action is allowed, but the agent never `gh pr review --approve` on its own work.

## When Not To Use This

- The user only wants investigation, a plan, or a review.
- The work needs design discussion before implementation.
- A PR link triage shows no actionable findings.
