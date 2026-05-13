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

## Argument modes

- **`/clud-issue-triage <num>` or `<url>`** → single-issue mode. Main agent does the work directly (no sub-agent — overkill for one issue).
- **`/clud-issue-triage`** (no arg) → bulk mode, scoped to issues created in the **last 7 days**.
- **`/clud-issue-triage all`** → bulk mode, scoped to **all OPEN issues** in the repo.

## Single-issue workflow

1. **Resolve the input.** Accept a bare number, a `#NNN`, or a full GitHub URL. `gh issue view <num> --json number,state,title,body,url,labels,closedByPullRequestsReferences,timelineItems`. If the call fails, say so and stop.
2. **If already CLOSED** → don't re-close. But still run step 5 (CodeRabbit follow-ups) against any linked PRs, then report.
3. **Find resolving PRs.** Scan `closedByPullRequestsReferences`, the timeline `cross-referenced` events, and `gh search prs "Closes #<num>" OR "Fixes #<num>" OR "Resolves #<num>" --repo <owner>/<repo>`. For each candidate:
   - `gh pr view <pr> --json state,merged,mergeCommit,baseRefName,files`.
   - Confirm `merged == true` AND `baseRefName == <default-branch>` (else the code didn't reach main — see `/clud-pr` step 3).
   - Read the diff (`gh pr diff <pr>`) and confirm it actually implements the issue's acceptance criteria. Don't trust the PR title or "Closes #" mention alone.
4. **Decide closure.** Bias toward caution:
   - Resolved unambiguously (merged + on default branch + diff matches criteria) → `gh issue close <num> --comment "Resolved by #<pr>. Verified: <one-line evidence>."`
   - Anything ambiguous (partial implementation, stack-base merge, criteria not fully met, no clear PR) → leave open. Don't comment unless the user asked you to triage-comment.
5. **Scan for un-addressed CodeRabbit comments.** For every PR linked to this issue (resolving or merely referencing):
   - `gh api repos/<owner>/<repo>/pulls/<pr>/comments` (review-thread comments) and `gh pr view <pr> --json reviews,comments`.
   - Filter authors matching `coderabbitai*` (`coderabbitai`, `coderabbitai[bot]`).
   - Drop nits, praise, "consider", and threads marked `outdated` or `resolved: true`.
   - Group remaining comments by file/topic.
6. **File follow-up issues silently.** For each cluster of substantive un-addressed CodeRabbit comments, `gh issue create --title "follow-up: <topic> from #<pr>" --body ...`. The body must include: source PR link, source comment permalinks, the CodeRabbit suggestion verbatim, and a one-line "why this matters" framing. Label `coderabbit-followup` if the repo has it; otherwise skip the label.
7. **Report.** Output exactly:
   - One line: closed (yes/no) + one-line evidence or one-line reason left open.
   - One line per follow-up issue filed: `#<num> — <title> — <url>`.
   - Nothing else.

## Bulk workflow (no arg or `all`)

1. **Enumerate candidates (main agent).**
   - No arg → `gh issue list --state open --limit 200 --search "created:>=$(date -d '7 days ago' --iso-8601 2>/dev/null || date -v-7d +%Y-%m-%d)" --json number,title,labels,createdAt,updatedAt`.
   - `all` → `gh issue list --state open --limit 1000 --json number,title,labels,createdAt,updatedAt`.
2. **Filter (main agent, fast pass).** Drop issues that are obviously not triage candidates: anything labeled `discussion`, `meta`, `roadmap`, `wontfix`, `duplicate`, `question`; anything updated in the last 24h (active conversation); anything with zero linked PRs/cross-references. The remainder is the work set.
3. **Stale-worktree prompt + .gitignore gate.** Same as `/clud-pr`'s **Worktree workspace** section. Confirm `.gitignore` covers `.claude/`. Ask the user about deleting any stale `.claude/worktrees/triage-*` older than 24h before starting.
4. **Plan worktrees.** One worktree per issue: `.claude/worktrees/triage-<num>/`. Branch off `origin/<default>` (read-only — these worktrees only inspect, they don't commit code). `git fetch origin && git worktree add --detach .claude/worktrees/triage-<num> origin/<default>` for each.
5. **Dispatch sub-agents in parallel.** Single tool-use block, one Agent call per issue. Each sub-agent gets:
   - The issue number and URL
   - The repo `<owner>/<repo>`
   - Its dedicated worktree path
   - Instruction: run the **Single-issue workflow** (steps 1–6 above) for this issue, in this worktree, and return a structured report (closed: bool, reason, follow-ups: list of {num, title, url})
   - Hard constraint: do not touch any path outside the assigned worktree; do not modify code in the main checkout
6. **Aggregate.** Collect every sub-agent's report. Tear down every worktree (`git worktree remove .claude/worktrees/triage-<num>` per issue). Confirm `git worktree list` shows none of them and `.claude/worktrees/` is empty (or holds only unrelated entries).
7. **Report (main agent).** Two sections:
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
