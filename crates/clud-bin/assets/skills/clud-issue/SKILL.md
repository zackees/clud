---
name: clud-issue
description: File a deeply-researched GitHub issue via investigate → interview → investigate → post, returning a summary plus the issue URL.
triggers:
  - When the user types "/clud-issue" with a topic or problem statement
  - When the user asks to "file an issue with research" or "open an issue after investigating"
  - When the user wants to draft an issue but needs the agent to clarify scope first
---
<!-- managed-by: clud -->

# /clud-issue

File a GitHub issue informed by real research and a real conversation. Four hard rules:

1. **Two investigation rounds, one interview in between** — never skip the interview, never post after a single pass.
2. **Post the issue** — finish with `gh issue create`. The deliverable is an issue URL, not a draft in chat.
3. **Surface strong duplicates only** — search existing issues; mention related issues *only* when similarity is strong (same component + overlapping intent). Don't pad the report with weak matches.
4. **Summary + URL last.** End with a short summary of what was filed and the URL — nothing else.

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

If the issue is for a bug fix or feature implementation, acceptance criteria must require RED -> GREEN evidence: a focused failing test/repro before coding, followed by the implementation that turns that signal green.

## Workflow

1. **Round 1 investigation (silent prep).** Read the topic. Skim the relevant code paths, existing docs, and recent git history to form a working model: what the user likely means, what's already in place, what's missing, what the obvious unknowns are. One round — don't spiral.
2. **Interview mode.** Ask the user clarifying questions to nail scope, constraints, and acceptance criteria. For each question you can answer well from the code or from public knowledge, *offer your best-judgement answer* alongside the question and ask the user to confirm or override. Batch related questions; don't drip-feed. Keep going until ambiguity is resolved — don't post a vague issue.
3. **Round 2 investigation.** With the user's answers in hand, do the deeper dig: confirm file paths, identify touch points, note prior art, list risks/edge cases. This round informs the actual issue body.
4. **Search for duplicates.** `gh issue list --search "<keywords>" --state all` (open + closed). Only flag issues with strong similarity — same component *and* overlapping intent. Weak keyword matches don't count.
5. **Draft the issue.** Title in conventional style (`feat:`, `fix:`, `chore:`, etc.). Body sections: **Context**, **Proposal**, **Acceptance criteria**, **Open questions** (if any remain), **Related issues** (only if strong matches found). For bug/feature work, acceptance criteria must include RED -> GREEN test evidence. No filler.
6. **Post.** `gh issue create --title "..." --body "$(cat <<'EOF' ... EOF)"`. Use a heredoc so formatting survives.
7. **Report.** Give the user: a 2-3 sentence summary of what was filed, then the issue URL. If strong related issues exist, mention them in one line above the URL. Nothing else.

## Failure modes to avoid

- **Skipping the interview.** Even "obvious" topics have hidden constraints. The interview is mandatory.
- **Posting before round 2.** Round 1 is for forming questions. Round 2 is for forming the issue. Don't conflate them.
- **Padding "Related issues" with weak matches.** A keyword overlap isn't a related issue. Only flag genuine overlap.
- **Vague acceptance criteria.** If the issue can't be closed objectively, the criteria are wrong. Rewrite them.
- **Posting without confirming.** Show the user the drafted title + body before `gh issue create` and let them adjust.

## When NOT to use this

- The user already has a clear, well-scoped issue draft — just file it directly.
- The work is trivial enough to implement immediately (skip the issue, do `/clud-pr` style work).
- The topic is a question, not a request for tracked work — answer it instead of filing.
