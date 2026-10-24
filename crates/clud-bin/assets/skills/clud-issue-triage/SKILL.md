---
name: clud-issue-triage
description: Triage GitHub issues — close ones that are absolutely resolved, and silently file follow-up issues for un-addressed CodeRabbit comments. Single issue, last week, or all (parallel read-only sub-agents, no worktrees).
triggers:
  - When the user types "/clud-issue-triage" with or without an issue number/URL
  - When the user types "/clud-issue-triage all"
  - When the user asks to "triage issues", "sweep stale issues", or "clean up the issue tracker"
---
<!-- managed-by: clud -->

# /clud-issue-triage

Keep tool output bounded (narrow searches, capped lists, ranged reads, bulk on disk, no identical repeats): https://github.com/zackees/clud/blob/main/docs/architecture/bounded-output.md

Triage GitHub issues for closeable resolution and un-addressed CodeRabbit follow-ups. Four hard rules:

1. **Bias toward caution on closing.** Default action is *leave open*. Only close when the resolution is unambiguous: a merged PR landed code on the default branch and that code clearly satisfies the issue's acceptance criteria. When in doubt, don't close.
2. **File follow-ups without asking.** Un-addressed CodeRabbit comments → file a new follow-up issue immediately, no user prompt. At the end, hand the user the list of follow-ups you filed.
3. **Bulk mode = main scans, sub-agents triage in parallel.** The main agent never does per-issue triage in bulk mode. It enumerates and filters the candidate set, then dispatches one sub-agent per issue in a single tool-use block.
4. **No worktrees, no checkouts.** Triage only reads, so it never creates a worktree or a clone (#1971). Read code at a revision instead: `git fetch origin` once, then `git show origin/<default>:<path>`, `git grep <pattern> origin/<default> -- <path>` and `git log origin/<default> -- <path>`, plus `gh` for PRs and issues. A worktree costs a cold Cargo target on Rust repos (see [[clud-git]]'s allowed conditions); triage meets none of them.

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
   - Confirm `merged == true` AND `baseRefName == <default-branch>` (else the code didn't reach main — a stack-base merge doesn't count).
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
2. **Filter (main agent, fast pass).** Drop issues that are obviously not triage candidates: anything labeled `grind:on-feature` (see Failure modes), a `grind:followup` issue that is not yet eligible (see Failure modes), `discussion`, `meta`, `roadmap`, `wontfix`, `duplicate`, `question`; anything updated in the last 24h (active conversation); anything with zero linked PRs/cross-references. The remainder is the work set.
3. **Set the goal.** With `<N>` candidates known, invoke `/goal Triage <N> candidate issues; for each: close if unambiguously resolved or file CodeRabbit follow-ups, then report aggregate Closed/Follow-ups counts ` so the harness Stop hook blocks until the aggregate report is emitted. If the filtered set is empty, report that and clear the goal — do not set it.
4. **Fetch once.** `git fetch origin` in the current checkout, so every sub-agent reads the same `origin/<default>` revision. Do not switch branches or touch the working tree.
5. **Resolve the revision.** `git rev-parse origin/<default>` gives the SHA every sub-agent reads.
6. **Dispatch sub-agents in parallel.** Single tool-use block, one Agent call per issue. Each sub-agent gets:
   - The issue number and URL
   - The repo `<owner>/<repo>`
   - The revision SHA, and the rule to read code only with `git show <sha>:<path>`, `git grep <pattern> <sha>` and `git log <sha> -- <path>`
   - Instruction: run the **Single-issue workflow** (steps 1–7 above, skipping the per-issue `/goal` step since the bulk goal already covers it) for this issue and return a structured report (closed: bool, reason, follow-ups: list of {num, title, url})
   - Hard constraint: create no worktree or clone, do not switch branches, and do not modify the checkout
7. **Aggregate.** Collect every sub-agent's report.
8. **Report (main agent).** Two sections:
   - **Closed (N):** one line per closed issue: `#<num> — <title> — resolved by #<pr>`.
   - **Follow-ups filed (M):** one line per filed follow-up: `#<num> — <title> — <url>`.
   - If both sections are empty, say so in one line. Nothing else.

## Failure modes to avoid

- **Touching a `grind:on-feature` issue.** Never pick up or close an issue labelled `grind:on-feature`, in single-issue or bulk mode: its code sits on an open `/grind` feature branch, and the feature PR's `Closes` line closes it when that PR merges into the default branch. Report it as left open.
- **Picking up a `grind:followup` issue too early.** A `/grind` run files problems it found as issues labelled `grind:followup`, with a body marker `<!-- grind:followup meta=<T> stage=<bugs|feature> feature-pr=<n|none> -->`. It is eligible only when the marker says `stage=bugs`, or its `feature-pr=<n>` PR is merged (`gh pr view <n> --json state -q .state` prints `MERGED`); until then its context sits on an open feature branch, so skip it and report it as left open. Once eligible it is an ordinary issue. Never attach one as a sub-issue of the meta issue.
- **Closing on weak evidence.** A PR that merely *mentions* the issue isn't resolution. Read the diff, match it to the criteria, or leave the issue open.
- **Closing on a stack-base merge.** A merged PR with `baseRefName != <default-branch>` did NOT land on main. Treat as not-resolved.
- **Asking before filing follow-ups.** The skill's job is to file silently. Asking defeats the point.
- **Counting CodeRabbit nits as un-addressed.** Praise, "nit:", and "consider" suggestions don't get follow-ups. Substantive threads only.
- **Filing duplicate follow-ups.** Before `gh issue create`, search: `gh issue list --search "from #<pr> coderabbit-followup" --state all`. Skip if a matching follow-up already exists.
- **Doing per-issue triage on the main agent in bulk mode.** Bulk mode = scan + dispatch + aggregate. Triage is sub-agent work, in parallel.
- **Creating a worktree or checkout to read code.** Triage reads through `git show` / `git grep` at a revision. A per-issue worktree on a Rust repo costs a cold target build and about 0.9 GB each, and buys nothing a read needs.
- **Touching the main checkout from a sub-agent.** Sub-agents only read. They do not commit, edit, switch branches or stash.

## When NOT to use this

- The user wants to file a brand-new issue → `/clud-issue` instead.
- The user wants to implement an issue → `/goal` instead.
- The repo's issue tracker is mostly used for discussion (not work tracking) — closing rules don't apply cleanly.
