<!-- managed-by: clud -->
---
name: clud-pr
description: Implement a GitHub issue via parallel sub-agents and ship as a single PR with a clean working tree.
triggers:
  - When the user asks to implement a GitHub issue
  - When the user says "ship", "do-pr", "/clud-pr", or references an issue URL/number with intent to deliver
---

# /clud-pr

Implement a GitHub issue end-to-end using parallel sub-agents, then push as a single PR. Three hard rules:

1. **Parallel sub-agents** — decompose work into independent file-scoped subtasks and dispatch sub-agents concurrently (single message, multiple Agent tool calls). Sequential implementation defeats the skill.
2. **Push the PR** — finish with `gh pr create`. Local commits don't count; the deliverable is a PR URL.
3. **No files left behind** — `git status` must be clean before push. Every untracked or modified file gets a deliberate decision: commit if it belongs in source, delete if it's tmp/scratch.

## Workflow

1. **Read the issue.** `gh issue view <num>` (or fetch the URL). Extract acceptance criteria, in-scope vs out-of-scope, touch points.
2. **Verify the issue is OPEN.** Check status and recent git log. If it was already implemented, surface that and stop — don't redo closed work.
3. **Branch off main.** `git fetch origin main && git checkout -b feat/<short-name> origin/main`. Never branch off a stale local main.
4. **Plan decomposition.** Identify file-scoped chunks that can run in parallel without merge conflicts. If everything must touch the same file, parallel agents won't help — say so and run sequentially.
5. **Dispatch parallel agents.** One Agent tool call per independent chunk, all in a single tool-use block so they run concurrently. Each agent gets: the issue text, its specific scope, the files it owns, and an explicit constraint not to touch files outside that scope.
6. **Lint + test.** Run the repo's lint and test commands (e.g. `bash lint`, `bash test`). Both green before push. Never `--no-verify`; if a hook fails, fix the underlying cause.
7. **Clean tree gate.** Run `git status`. For every untracked or modified file:
   - Belongs in source → `git add` and commit
   - Tmp / scratch / build artifact → delete (`rm`) before commit
   No exceptions. The tree must be clean before push.
8. **Commit.** Conventional commit format. Reference the issue (`Closes #<num>` in commit body or PR body).
9. **Push + PR.** `git push -u origin <branch>`, then `gh pr create` with a body that links the issue, summarizes what each parallel agent did, and includes a test plan checklist.

## Failure modes to avoid

- **Sequential masquerading as parallel.** If your "parallel agents" depend on each other's output, they aren't parallel. Restructure or admit it's sequential.
- **Pushing with a dirty tree.** Never `git stash` away stray files to fake a clean status. Each file gets a commit-or-delete decision.
- **Skipping lint/test.** Mandatory after code changes. Don't push red.
- **Implementing closed issues.** Verify state before starting.
- **Multiple PRs for one issue.** The deliverable is a single PR. If the work is too large for one PR, raise that with the user before splitting.

## When NOT to use this

- Single-file changes where parallelism gives no benefit
- Issues that need design discussion before implementation
- Work crossing major architectural boundaries (do design first)
