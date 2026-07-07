---
name: clud-issue-triage
description: Triage GitHub issues — close ones that are absolutely resolved, and silently file follow-up issues for un-addressed CodeRabbit comments. Single issue, last week, or all (parallel sub-agents in worktrees).
triggers:
  - When the user types "/clud-issue-triage" with or without an issue number/URL
  - When the user types "/clud-issue-triage all"
  - When the user asks to "triage issues", "sweep stale issues", or "clean up the issue tracker"
---
<!-- managed-by: clud -->

# /clud-issue-triage

Triage GitHub issues for closeable resolution and un-addressed CodeRabbit follow-ups. Four hard rules:

1. **Bias toward caution on closing.** Default action is *leave open*. Only close when the resolution is unambiguous: a merged PR landed code on the default branch and that code clearly satisfies the issue's acceptance criteria. When in doubt, don't close.
2. **File follow-ups without asking.** Un-addressed CodeRabbit comments → file a new follow-up issue immediately, no user prompt. At the end, hand the user the list of follow-ups you filed.
3. **Bulk mode = main scans, sub-agents triage in parallel git worktrees.** The main agent never does per-issue triage in bulk mode. It enumerates and filters the candidate set, then dispatches one sub-agent per issue in a single tool-use block. Each sub-agent runs in its own `.claude/worktrees/triage-<num>/`.
4. **Nothing left in the repo when done.** Every worktree gets `git worktree remove`'d after its sub-agent finishes. Same `.gitignore` gate and stale-worktree rules as `/clud-pr`.

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
- `gh issue create` ↔ `glab issue create` ↔ `tea issues create` ↔ Bitbucket REST: `POST /repositories/<o>/<r>/issues`

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

## Code Change Rule

When this skill files a follow-up issue for a bug fix or feature implementation, the follow-up must require RED -> GREEN evidence: a focused failing test/repro first, then the implementation that turns that signal green.

## Argument modes

- **`/clud-issue-triage <num>` or `<url>`** → single-issue mode. Main agent does the work directly (no sub-agent — overkill for one issue).
- **`/clud-issue-triage`** (no arg) → bulk mode, scoped to issues created in the **last 7 days**.
- **`/clud-issue-triage all`** → bulk mode, scoped to **all OPEN issues** in the repo.

## Single-issue workflow

1. **Resolve the input.** Accept a bare number, a `#NNN`, or a full GitHub URL. `gh issue view <num> --json number,state,title,body,url,labels,closedByPullRequestsReferences,timelineItems`. If the call fails, say so and stop.
2. **Set the goal.** Invoke `/goal Triage issue #<num>: close if unambiguously resolved, file CodeRabbit follow-ups for any un-addressed substantive threads, or report exactly why neither happened.` so the harness Stop hook blocks until the closure decision is made and follow-ups are filed. The goal auto-clears once the final report is emitted.
3. **If already CLOSED** → don't re-close. But still run step 6 (CodeRabbit follow-ups) against any linked PRs, then report.
4. **Find resolving PRs.** Scan `closedByPullRequestsReferences`, the timeline `cross-referenced` events, and `gh search prs "Closes #<num>" OR "Fixes #<num>" OR "Resolves #<num>" --repo <owner>/<repo>`. For each candidate:
   - `gh pr view <pr> --json state,merged,mergeCommit,baseRefName,files`.
   - Confirm `merged == true` AND `baseRefName == <default-branch>` (else the code didn't reach main — see `/clud-pr` step 3).
   - Read the diff (`gh pr diff <pr>`) and confirm it actually implements the issue's acceptance criteria. Don't trust the PR title or "Closes #" mention alone.
5. **Decide closure.** Bias toward caution:
   - Resolved unambiguously (merged + on default branch + diff matches criteria) → `gh issue close <num> --comment "Resolved by #<pr>. Verified: <one-line evidence>."`
   - Anything ambiguous (partial implementation, stack-base merge, criteria not fully met, no clear PR) → leave open. Don't comment unless the user asked you to triage-comment.
6. **Scan for un-addressed CodeRabbit comments.** For every PR linked to this issue (resolving or merely referencing):
   - `gh api repos/<owner>/<repo>/pulls/<pr>/comments` (review-thread comments) and `gh pr view <pr> --json reviews,comments`.
   - Filter authors matching `coderabbitai*` (`coderabbitai`, `coderabbitai[bot]`).
   - Drop nits, praise, "consider", and threads marked `outdated` or `resolved: true`.
   - Group remaining comments by file/topic.
7. **File follow-up issues silently.** For each cluster of substantive un-addressed CodeRabbit comments, `gh issue create --title "follow-up: <topic> from #<pr>" --body ...`. The body must include: source PR link, source comment permalinks, the CodeRabbit suggestion verbatim, a one-line "why this matters" framing, and RED -> GREEN acceptance criteria when the follow-up is a bug fix or feature implementation. Label `coderabbit-followup` if the repo has it; otherwise skip the label.
8. **Report.** Output exactly:
   - One line: closed (yes/no) + one-line evidence or one-line reason left open.
   - One line per follow-up issue filed: `#<num> — <title> — <url>`.
   - Nothing else.

## Bulk workflow (no arg or `all`)

1. **Enumerate candidates (main agent).**
   - No arg → `gh issue list --state open --limit 200 --search "created:>=$(date -d '7 days ago' --iso-8601 2>/dev/null || date -v-7d +%Y-%m-%d)" --json number,title,labels,createdAt,updatedAt`.
   - `all` → `gh issue list --state open --limit 1000 --json number,title,labels,createdAt,updatedAt`.
2. **Filter (main agent, fast pass).** Drop issues that are obviously not triage candidates: anything labeled `discussion`, `meta`, `roadmap`, `wontfix`, `duplicate`, `question`; anything updated in the last 24h (active conversation); anything with zero linked PRs/cross-references. The remainder is the work set.
3. **Set the goal.** With `<N>` candidates known, invoke `/goal Triage <N> candidate issues; for each: close if unambiguously resolved or file CodeRabbit follow-ups, then report aggregate Closed/Follow-ups counts and tear down every worktree.` so the harness Stop hook blocks until the aggregate report is emitted and every worktree is gone. If the filtered set is empty, report that and clear the goal — do not set it.
4. **Stale-worktree prompt + .gitignore gate.** Same as `/clud-pr`'s **Worktree workspace** section. Confirm `.gitignore` covers `.claude/`. Ask the user about deleting any stale `.claude/worktrees/triage-*` older than 24h before starting.
5. **Plan worktrees.** One worktree per issue: `.claude/worktrees/triage-<num>/`. Branch off `origin/<default>` (read-only — these worktrees only inspect, they don't commit code). `git fetch origin && git worktree add --detach .claude/worktrees/triage-<num> origin/<default>` for each.
6. **Dispatch sub-agents in parallel.** Single tool-use block, one Agent call per issue. Each sub-agent gets:
   - The issue number and URL
   - The repo `<owner>/<repo>`
   - Its dedicated worktree path
   - Instruction: run the **Single-issue workflow** (steps 1–7 above, skipping the per-issue `/goal` step since the bulk goal already covers it) for this issue, in this worktree, and return a structured report (closed: bool, reason, follow-ups: list of {num, title, url})
   - Hard constraint: do not touch any path outside the assigned worktree; do not modify code in the main checkout
7. **Aggregate.** Collect every sub-agent's report. Tear down every worktree (`git worktree remove .claude/worktrees/triage-<num>` per issue). Confirm `git worktree list` shows none of them and `.claude/worktrees/` is empty (or holds only unrelated entries).
8. **Report (main agent).** Two sections:
   - **Closed (N):** one line per closed issue: `#<num> — <title> — resolved by #<pr>`.
   - **Follow-ups filed (M):** one line per filed follow-up: `#<num> — <title> — <url>`.
   - If both sections are empty, say so in one line. Nothing else.

## Failure modes to avoid

- **Closing on weak evidence.** A PR that merely *mentions* the issue isn't resolution. Read the diff, match it to the criteria, or leave the issue open.
- **Closing on a stack-base merge.** A merged PR with `baseRefName != <default-branch>` did NOT land on main. Treat as not-resolved.
- **Asking before filing follow-ups.** The skill's job is to file silently. Asking defeats the point.
- **Counting CodeRabbit nits as un-addressed.** Praise, "nit:", and "consider" suggestions don't get follow-ups. Substantive threads only.
- **Filing duplicate follow-ups.** Before `gh issue create`, search: `gh issue list --search "from #<pr> coderabbit-followup" --state all`. Skip if a matching follow-up already exists.
- **Doing per-issue triage on the main agent in bulk mode.** Bulk mode = scan + dispatch + aggregate. Triage is sub-agent work, in worktrees, in parallel.
- **Letting worktrees leak.** Every dispatched worktree gets removed at the end, even on partial failure. Verify with `git worktree list`.
- **Touching the main checkout from a sub-agent.** Sub-agents only read/inspect inside their assigned `.claude/worktrees/triage-<num>/`. They do not commit, edit, or `cd` outside.

## When NOT to use this

- The user wants to file a brand-new issue → `/clud-issue` instead.
- The user wants to implement an issue → `/clud-pr` instead.
- The repo's issue tracker is mostly used for discussion (not work tracking) — closing rules don't apply cleanly.
