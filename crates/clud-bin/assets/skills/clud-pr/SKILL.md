---
name: clud-pr
description: Implement a GitHub issue (or triage a PR link) inside a .claude/ worktree and ship as a single PR with no files left in the repo.
triggers:
  - When the user asks to implement a GitHub issue
  - When the user passes a PR URL/number to "/clud-pr" (triage mode)
  - When the user says "ship", "do-pr", "/clud-pr", or references an issue URL/number with intent to deliver
---
<!-- managed-by: clud -->

# /clud-pr

Implement a GitHub issue end-to-end using parallel sub-agents inside a disposable `.claude/` worktree, then push as a single PR. Three hard rules:

1. **Parallel sub-agents** — decompose work into independent file-scoped subtasks and dispatch sub-agents concurrently (single message, multiple Agent tool calls). Sequential implementation defeats the skill.
2. **Push the PR** — finish with `gh pr create`. Local commits don't count; the deliverable is a PR URL.
3. **Nothing left in the repo after push** — the worktree itself, untracked files, scratch files, build artifacts: all gone. After `gh pr create` succeeds, the main checkout's `git status` AND the worktree directory must both be clean (worktree removed). Never leave anything behind.

## Worktree workspace (do all PR work here)

Do not commit on the main checkout. Create a git worktree under `.claude/worktrees/<branch-name>/` and work there.

1. **`.gitignore` gate.** The worktree path must be ignored by git so the parent checkout never sees it as untracked. Check `.gitignore` for an entry that covers `.claude/` (e.g. `.claude/`, `.claude/**`, or `/.claude/`).
   - If present → fine, continue.
   - If missing → **delete any existing `.claude/worktrees/`** to keep the repo clean, then either (a) ask the user for permission to add `.claude/` to `.gitignore`, or (b) fall back to a sibling path outside the repo (`../<repo>-wt-<branch>/`). Do NOT silently create a worktree at an un-ignored path.
2. **Stale-worktree prompt.** Before creating the new worktree, scan `.claude/worktrees/*` for subdirectories whose mtime is older than 24 hours. If any exist, list them and ask the user:
   > "Found N stale worktree(s) in `.claude/worktrees/` (older than 1 day): <list>. Delete them before I start?"
   On "yes" → `git worktree remove --force <path>` for each (falling back to `rm -rf` if `git worktree remove` rejects it because the worktree is corrupt). On "no" → leave them alone and continue.
3. **Create the worktree.** `git fetch origin main && git worktree add -b feat/<short-name> .claude/worktrees/<branch> origin/main`. All subsequent edits, commits, lint, and test runs happen inside that path. Never `cd` back to the main checkout for this PR's work.
4. **Tear down after push.** After `gh pr create` returns successfully, run `git worktree remove .claude/worktrees/<branch>` (use `--force` only if needed). Confirm with `git worktree list` that the entry is gone and `ls .claude/worktrees/` no longer contains the directory. The user's repo state should look identical to before `/clud-pr` ran.

## Mode selection

Look at the input first:

- **Issue URL / number / freeform task** → run the **issue → PR workflow** below.
- **PR URL / number** → run **PR triage mode** instead. Don't start branching or coding until triage decides what (if anything) to do.

## PR triage mode (when `/clud-pr` is given a PR link)

1. **Verify the PR exists.** `gh pr view <url-or-num> --json number,state,title,url,headRefName,mergedAt,closedAt,author`. If the call fails (404, wrong repo, malformed URL), say so and stop — don't guess.
2. **Branch on state.**
   - **OPEN** → report the PR's current state (CI status, review status, mergeability) and ask the user what they want done. Don't implement anything unprompted.
   - **MERGED** → say so and stop. Follow-up work belongs in a new PR/issue, not this one.
   - **CLOSED (not merged)** → continue with the checks below.
3. **Check for CodeRabbit review comments.** `gh pr view <num> --json reviews,comments` and `gh api repos/<owner>/<repo>/pulls/<num>/comments` (review-thread comments live there, not on the issue). Filter for author logins matching `coderabbitai*` (bot accounts: `coderabbitai`, `coderabbitai[bot]`). Count actionable comments — skip "LGTM" / praise / nit-only threads.
4. **Check for failing GH Actions.** `gh pr checks <num>` (or `gh run list --branch <headRefName> --limit 5`). Note which workflows failed and on which commit.
5. **Check for an existing follow-up PR.** `gh pr list --state all --search "<headRefName> OR #<num>"` and scan the closed PR's timeline (`gh api repos/<owner>/<repo>/issues/<num>/timeline`) for `cross-referenced` events linking to a later PR by the same author or touching the same files. If one exists and looks like it addresses the comments / fixes the CI, surface it — the user may not need to redo the work.
6. **Report findings and ask, one question per finding.** Examples:
   - "I see N actionable CodeRabbit comments on this closed PR (summary: ...). Want me to address them in a new PR?"
   - "CI failed on the `<workflow>` job (last failure: <one-line summary>). Want me to fix it in a new PR?"
   - "I found a follow-up PR #<num> that looks like it already addresses the CodeRabbit feedback. Want me to confirm and stop, or continue regardless?"
   Wait for the user's answer before doing anything destructive or branch-creating.
7. **On user "yes" → switch to issue → PR workflow,** but: branch off `origin/main` inside a fresh `.claude/worktrees/<branch>` (per the worktree section above), and reference the closed PR in the new PR body (`Refs #<num>` and a short note: "Addresses CodeRabbit feedback from #<num>" or "Fixes CI failure from #<num>"). Cherry-pick only the closed PR's commits the user explicitly asks to keep.

## Workflow (issue → PR)

1. **Read the issue.** `gh issue view <num>` (or fetch the URL). Extract acceptance criteria, in-scope vs out-of-scope, touch points.
2. **Verify the issue is OPEN.** Check status and recent git log. If it was already implemented, surface that and stop — don't redo closed work.
3. **Check if a PR already resolves it.** Even an OPEN issue may already be done — the issue just wasn't closed. Look for resolving PRs:
   - `gh issue view <num> --json closedByPullRequestsReferences,timelineItems` and scan the timeline for `cross-referenced` PRs.
   - `gh pr list --state all --search "<num> in:title,body"` and `gh search prs "Closes #<num>" OR "Fixes #<num>" OR "Resolves #<num>" --repo <owner>/<repo>`.
   - For any candidate PR found: confirm it actually addresses the issue's acceptance criteria (read its diff/description), not just mentions the number.
   - **Verify the code reached main.** `gh pr view <pr> --json state,merged,mergeCommit,baseRefName`. If `baseRefName` isn't the repo's default branch, the merge only landed on a stack base — the code does NOT yet reach main. Treat the issue as **not resolved** and continue to step 4 (dependency walk).
   If a PR genuinely resolves the issue *and* its merge landed on main, tell the user: "Issue #<num> looks resolved by PR #<pr> (merged to <branch>). Want me to close issue #<num> with a comment linking the PR?" On "yes" → `gh issue close <num> --comment "Resolved by #<pr>."` Either way, stop here — don't start a redundant new PR.
4. **Dependency walk (only when the resolving PR is blocked by upstream work).** If step 3 found a PR that addresses the issue but the code hasn't reached main (stack base, linked dependent PR, "blocked by #X" reference, etc.), enumerate the blockers before doing anything else:
   - Read the resolving PR's `baseRefName` and look up the PR/issue that owns that branch. Recurse until you hit `main`.
   - Pull `gh issue view <num> --json closedByPullRequestsReferences,timelineItems` and parse the issue body for `Depends on #<n>`, `Blocked by #<n>`, `Part of #<n>`, and explicit `<owner>/<repo>#<n>` cross-repo references.
   - Group findings by repo:
     - **Same repo** → these are the priority. List them to the user in dependency order and default to taking them on first. Phrase as a confirmation, not an open question: "To land #<num>, these must merge first: #A, #B (same repo). I'll take them in that order — confirm or override."
     - **Other repos** → never auto-tackle. Ask explicitly, one prompt per repo: "Issue #<num> also depends on work in `<owner>/<repo>` (issues: #X, #Y). Want me to take those on too?"
   - Dispatch:
     - Same-repo dependencies → run them through the workflow sequentially (each lands on main before the next starts), unless they touch fully independent files (then parallel — same rule as step 6).
     - **Cross-repo dependencies → always parallel.** One sub-agent per repo, all dispatched in a single tool-use block. Each sub-agent runs the full `/clud-pr` workflow scoped to its repo, with its own worktree under that repo's `.claude/worktrees/<branch>/`. Sub-agents must not cross repo boundaries.
   - Only after the upstream work is done (or explicitly deferred by the user) come back to the originally-requested issue.
5. **Set up the worktree.** Run the `.gitignore` gate, the stale-worktree prompt, and `git worktree add` from the **Worktree workspace** section above. All work from here on is inside `.claude/worktrees/<branch>/`.
6. **Plan decomposition.** Identify file-scoped chunks that can run in parallel without merge conflicts. If everything must touch the same file, parallel agents won't help — say so and run sequentially.
7. **Dispatch parallel agents.** One Agent tool call per independent chunk, all in a single tool-use block so they run concurrently. Each agent gets: the issue text, its specific scope, the files it owns, the worktree path it must operate in, and an explicit constraint not to touch files outside that scope.
8. **Lint + test.** Run the repo's lint and test commands (e.g. `bash lint`, `bash test`) inside the worktree. Both green before push. Never `--no-verify`; if a hook fails, fix the underlying cause.
9. **Clean tree gate.** Run `git status` inside the worktree. For every untracked or modified file:
   - Belongs in source → `git add` and commit
   - Tmp / scratch / build artifact → delete (`rm`) before commit
   No exceptions. The worktree must be clean before push.
10. **Commit.** Conventional commit format. Reference the issue (`Closes #<num>` in commit body or PR body).
11. **Push + PR.** `git push -u origin <branch>`, then `gh pr create` with a body that links the issue, summarizes what each parallel agent did, and includes a test plan checklist.
12. **Tear down the worktree.** `git worktree remove .claude/worktrees/<branch>`. Confirm `git worktree list` no longer shows it and `.claude/worktrees/` doesn't contain it. Then verify the *main checkout's* `git status` is clean — nothing the worktree did should have leaked there.

## Failure modes to avoid

- **Sequential masquerading as parallel.** If your "parallel agents" depend on each other's output, they aren't parallel. Restructure or admit it's sequential.
- **Pushing with a dirty tree.** Never `git stash` away stray files to fake a clean status. Each file gets a commit-or-delete decision.
- **Skipping lint/test.** Mandatory after code changes. Don't push red.
- **Implementing closed issues.** Verify state before starting.
- **Implementing an issue that already has a resolving PR.** Even open issues can be already-done. Search for resolving PRs and offer to close the issue instead of starting parallel work.
- **Calling an issue "resolved" when the PR didn't reach main.** A merged PR whose `baseRefName` isn't main only landed on a stack base — the code isn't on main yet. Walk the dependency chain instead.
- **Auto-tackling cross-repo dependencies.** Same-repo prerequisites are taken on by default; cross-repo ones require an explicit user "yes" — one prompt per repo. Never silently spawn work in a repo the user didn't sign off on.
- **Sequential cross-repo work.** When the user approves multiple cross-repo dependencies, dispatch one sub-agent per repo in a single tool-use block. Doing them serially defeats the parallelism rule.
- **Multiple PRs for one issue.** The deliverable is a single PR. If the work is too large for one PR, raise that with the user before splitting.
- **Acting on a PR link without triage.** Never branch / code / push when the input is a PR URL until triage has run and the user has answered the findings.
- **Pattern-matching CodeRabbit nits as actionable.** Praise, "nit:", and "consider" suggestions don't count as findings unless the user asks. Count only substantive review comments.
- **Reopening already-fixed work.** If a follow-up PR exists that addresses the comments / CI, surface it before offering to redo the work.
- **Committing on the main checkout.** All commits happen inside `.claude/worktrees/<branch>/`. The main checkout stays untouched.
- **Creating a worktree at an un-ignored path.** If `.gitignore` doesn't cover `.claude/`, delete the worktree dir and resolve the gitignore situation before continuing — never let worktree internals show up as untracked files in the parent repo.
- **Skipping `git worktree remove` after push.** The deliverable is "PR pushed AND repo state restored." A leftover worktree is a leftover file.
- **Auto-deleting stale worktrees without asking.** Even old worktrees may hold the user's in-progress work. Always prompt before removing.

## When NOT to use this

- Single-file changes where parallelism gives no benefit
- Issues that need design discussion before implementation
- Work crossing major architectural boundaries (do design first)
- A PR link whose triage shows no actionable findings (nothing to do — say so and stop)
