---
name: clud-do
description: Classify any GitHub URL, issue/PR number, or freeform task and route it to the right downstream skill (clud-fix for bug-shaped issues, clud-pr for features / freeform / PR triage). One-line dispatcher; no scope of its own.
triggers:
  - When the user says "/clud-do <url-or-task>"
  - When the user invokes "/loop clud-do <url>"
  - When the user gives an issue/PR URL but isn't sure which sub-skill to invoke
  - When a meta-skill or harness needs an entry point that classifies before dispatching
---
<!-- managed-by: clud -->

# /clud-do

Thin router on top of [[clud-fix]] and [[clud-pr]]. Reads any GitHub issue
URL, PR URL, issue/PR number, or freeform task; classifies the input;
dispatches to exactly one downstream skill; recovers cleanly when the first
dispatch was a mis-triage.

`/clud-do` does not code, does not branch, does not set `/goal`, and does
not own any worktree of its own. The child skill owns all of that.

For code changes, the child skill ([[clud-fix]] or [[clud-pr]]) preserves
RED -> GREEN: identify or add the focused failing test/repro first,
implement the scoped change, then rerun that focused signal until it
passes before broad gates. `/clud-do` itself never makes code changes,
so the rule applies through delegation — never directly.

## Input

- A GitHub issue URL such as `https://github.com/<owner>/<repo>/issues/<num>`.
- A GitHub PR URL such as `https://github.com/<owner>/<repo>/pull/<num>`.
- A bare `#<num>` (issue) or `!<num>` (PR) when the current checkout
  unambiguously identifies the repository. Resolve `<owner>/<repo>` with
  `gh repo view` before acting.
- A freeform task sentence such as `add a structured changelog generator`.

Optional user hint may prefix the argument: `/clud-do fix <url>` forces
the `/clud-fix` route; `/clud-do feat <url>` forces the `/clud-pr` route.
User hints win outright over every other classifier signal.

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

## Hard Rules

1. **Classify, dispatch, pass through. Never code, never branch, never set
   `/goal`.** The child skill owns its goal, worktree, and terminal sentinels.
2. **Re-entrant.** On every invocation, re-read external state (`gh issue
   view`, `gh pr view`). `/loop` will re-invoke; the second iteration's
   "open PR exists, drive to merge" is a different dispatch than the first
   iteration's "implement from scratch."
3. **One dispatch per invocation.** No fan-out. Meta issues route to
   `/clud-fix` and let its meta workflow handle the children.
4. **Refuse early on unreachable scope.** Borrow the gate from
   [[clud-fix]] (read-only repo, closed-source target, non-GitHub URL).
   Refuse with the unreachable-scope sentinel rather than entering a
   dispatch that will end in the child's own refusal.

## Input Classification

Read once with `gh issue view <num> --repo <owner>/<repo> --json
number,state,title,body,labels,comments,url,closedByPullRequestsReferences`
(or `gh pr view` for PR inputs). Then dispatch from this table:

| Input | State | Dispatch |
|---|---|---|
| Issue URL or bare `#N` | OPEN, bug-shaped | `/clud-fix <url>` |
| same | OPEN, feature-shaped | `/clud-pr <url>` |
| same | OPEN, meta/parent/burn-down | `/clud-fix <url>` (its meta workflow owns this) |
| same | OPEN, ambiguous | default to `/clud-fix` (stricter gate catches mis-routes; recovery branch below handles the rejection) |
| same | CLOSED, resolving PR merged | emit done-terminal (no dispatch) |
| same | CLOSED, no resolving PR | emit done-terminal |
| PR URL or bare `!N` | OPEN | `/clud-pr <pr-url>` (triage mode) |
| same | MERGED | emit done-terminal |
| same | CLOSED, not merged | `/clud-pr <pr-url>` (its CLOSED-not-merged path) |
| Freeform task sentence | n/a | `/clud-pr <text>` |
| Non-GitHub URL / Actions run / Gist / other | n/a | refuse with unreachable-scope sentinel |

Classify the issue as meta/parent/burn-down when labels, title, checklist
body, linked issue references, GitHub sub-issue metadata, or explicit user
wording identify a tracker — same rule [[clud-fix]] uses.

## Bug-vs-Feature Classifier

[[clud-fix]] does NOT require a `bug` label or `[bug]` title prefix — its
gate is purely content-based. So `/clud-do` treats labels and titles as
**signals, not requirements**. Apply in priority order, highest signal
first; stop at the first signal that resolves the input:

1. **User hint** in the invocation (`/clud-do fix <url>`,
   `/clud-do feat <url>`) wins outright.
2. **Labels** — `bug` → fix; `enhancement` / `feature` / `feat` → pr.
3. **Title prefix** — conventional commit prefixes (`fix:`, `feat:`).
4. **Body shape** — "Steps to reproduce" / "Expected vs actual" → fix;
   "Acceptance criteria" / "Proposal" / "Goal" / "Definition of done"
   without a reproduction → feature.
5. **Recent comments** mentioning a clear repro (command, stack trace,
   wrong output) — promote to bug-shaped even if the body was thin.
6. **Default to `/clud-fix`** when nothing matches. Its readiness gate
   becomes the secondary classifier and the recovery branch below catches
   the mis-route cleanly.

## Recovery Branch

`/clud-fix` emits a precise terminal when its intake gate rejects:

```text
<clud-fix:terminal kind=investigation-report-posted reason=intake-failed url=<issue-url>>
```

If and only if the initial dispatch was `/clud-fix` AND its terminal was
**that exact sentinel**, `/clud-do` recovers:

1. **Read the investigation report `/clud-fix` just posted to the issue.**
   Use `gh issue view <num> --json comments --jq '.comments[-1].body'` (or
   filter for the most-recent `clud-fix intake gate failed` banner). The
   report enumerates which readiness items were missing
   (reproduction / symptom / criterion / scope).
2. **Re-classify with the post-rejection signal.** A feature-shaped
   recovery candidate has:
   - At least one of: explicit "Acceptance criteria" section, "Definition
     of done", "Proposal" section, `feat:` title, or `enhancement` /
     `feature` label that the initial pass missed because a stronger bug
     signal was present.
   - The `/clud-fix` investigation report cites missing reproduction OR
     missing symptom (the bug-specific items) but does NOT cite missing
     fixed criterion. The latter would mean even the feature shape is
     under-specified.
3. **One recovery attempt per loop iteration.** If the candidate
   qualifies, dispatch to `/clud-pr <url>` and emit:

   ```text
   <clud-do:terminal kind=recovered-to-clud-pr original=clud-fix reason=feature-shaped-issue url=<url>>
   ```

4. **If the candidate does NOT qualify** (no acceptance criteria AND no
   feature labels AND the report cites missing fixed criterion too), the
   issue is genuinely under-specified for either flow. Do NOT re-dispatch.
   Emit:

   ```text
   <clud-do:terminal kind=routing-failed reason=neither-bug-nor-feature-shape-clear url=<url>>
   ```

   Suggest the user add either "Steps to reproduce" (bug shape) or
   "Acceptance criteria" (feature shape), then stop.

5. **No re-recovery.** `/clud-pr`'s task-to-PR workflow is permissive —
   if it also refuses, that's a real refusal, not a triage error. Emit:

   ```text
   <clud-do:terminal kind=both-skills-refused url=<url>>
   ```

   Recovery is unidirectional (`/clud-fix` → `/clud-pr` only), cycle-free
   by construction.

**Other `/clud-fix` terminals are NOT recoverable** —
`unreachable-scope-refused` and `empty-children-report-posted` mean the
work genuinely can't be done in this repo; re-dispatching won't help.
Pass them through verbatim and emit the matching wrap-and-add sentinel.

## /loop Integration

The existing `/loop` dynamic-mode contract handles `/clud-do` with zero
`/loop` changes:

1. `/loop clud-do <url>` parses with rule 3 (no interval) → prompt =
   `clud-do <url>`.
2. **Iteration 1.** `/loop` wakes; invokes `/clud-do`. `/clud-do` reads
   issue state, classifies, dispatches to `/clud-fix` or `/clud-pr`. The
   child runs to *its* terminal (merged + closed + validated,
   investigation-report-posted, unreachable-scope-refused, PR-opened,
   etc.).
3. **Iteration N.** `/loop` wakes again; re-invokes `/clud-do`. `/clud-do`
   re-reads issue state — issue may now be CLOSED, or have an open PR, or
   have a merged PR awaiting validation. It re-classifies against the new
   state and either re-dispatches (e.g. "open PR exists → `/clud-pr` to
   drive merge") or emits a done-terminal.

**`/clud-do` is stateless across loop iterations.** No ledger, no
`.clud/do/<hash>.json`. The state of truth is GitHub. `/clud-fix`'s own
meta-workflow ledger is unchanged.

The loop's natural termination is the same as today: when `/clud-do`'s
done-terminal sentinel fires and the loop's prompt has nothing else to
do, the next iteration's `/clud-do` emits the same done-terminal again.
The loop user (or an outer `/goal`) can detect that and stop scheduling
further wake-ups.

## Terminal Sentinels — Wrap-and-Add

`/clud-do` emits both the child's verbatim sentinel AND a
`<clud-do:terminal …>` wrapper that includes the child's terminal as a
nested field:

```text
<clud-fix:terminal kind=investigation-report-posted reason=intake-failed url=...>
<clud-do:terminal kind=recovered-to-clud-pr original=clud-fix child-terminal=intake-failed url=...>
```

Outer `/goal` evaluators can match on `clud-do:terminal` exclusively
without learning every child skill's vocabulary; the transcript still
carries the precise child terminal for debugging. Belt-and-suspenders.

Sentinel kinds `/clud-do` emits:

| `kind=` | Meaning |
|---|---|
| `dispatched-to-clud-fix` | Initial dispatch to `/clud-fix` succeeded; child terminal passed through. |
| `dispatched-to-clud-pr` | Initial dispatch to `/clud-pr` succeeded; child terminal passed through. |
| `recovered-to-clud-pr` | `/clud-fix` rejected at intake; recovered by re-dispatching to `/clud-pr`. |
| `done-already` | Issue/PR is already in its terminal state (closed + resolving PR merged, PR merged); no dispatch needed. |
| `routing-failed` | Issue is under-specified for both bug and feature shapes. |
| `both-skills-refused` | Recovery dispatch to `/clud-pr` also refused. |
| `unreachable-scope-refused` | The reachable-scope gate fired before dispatching. |

Every sentinel includes `url=<issue-or-pr-url>` (or `task=<freeform>`
for freeform inputs). Emit the sentinel as the FINAL line of the
user-facing response.

## Diagnostic Surface

Every recovery iteration should print a one-paragraph diagnostic so the
user can see *why* the router second-guessed itself:

```text
/clud-do routed to /clud-fix (signal: no `feature` label, no `feat:` prefix,
body contained "Issue" keyword).
/clud-fix rejected at intake — investigation report at <comment-url>;
missing: concrete reproduction, falsifiable symptom.
Re-classifying: issue body has an explicit "Acceptance criteria" section
and three checkbox items. Re-dispatching to /clud-pr as a feature
implementation.
```

That's diagnostic AND trains the heuristic — next time the user types
`/clud-do <similar-url>` (or files the same shape of issue), they know
what to do. If the user disagrees with the recovery on a specific issue,
they can add a `bug` label so the next loop iteration's classifier picks
it up correctly.

## Failure Modes To Avoid

- **Re-recovery loops.** Recovery only flows `/clud-fix` → `/clud-pr`,
  never the other way. If `/clud-pr` also refuses, emit
  `both-skills-refused` and stop; do not retry.
- **Widening scope to manufacture a PR-able task.** If the issue is
  genuinely under-specified, `routing-failed` is the right terminal.
  Same trap as `/clud-fix` documents at its `/goal`-wedge failure mode.
- **Subsuming `/clud-issue-triage`.** Closed issues with resolving PRs
  route to `done-already`, not to a closure-audit. Suggest
  `/clud-issue-triage` and stop if the user wants a triage.
- **Replacing `/clud-fix` for users who know they want a bug fix.**
  `/clud-do` is an additive router; direct `/clud-fix <url>` calls
  continue to work unchanged. Do not insert `/clud-do` between the user
  and `/clud-fix` when they typed `/clud-fix` themselves.
- **Mutating the issue body.** `/clud-do` only reads; it does not edit
  the issue, add labels, or close it. Recovery posts no new comment of
  its own — the `/clud-fix` investigation report is the user-visible
  artifact.
- **Treating cached GitHub state as authoritative across loop
  iterations.** Always re-fetch. The whole point of the loop is to react
  to state changes.

## When Not To Use This

- The user explicitly typed `/clud-fix` or `/clud-pr` — honour the
  request. Use `/clud-do` only when the user invoked it directly or when
  the input shape genuinely needs classification.
- The user wants closure-audit / CodeRabbit-follow-up filing on a CLOSED
  issue — that's [[clud-issue-triage]].
- The user wants to file a new issue from scratch — that's
  [[clud-issue]].
- Bulk operations across many issues — that's
  [[clud-issue-triage]] in bulk mode, not `/clud-do`.
