---
name: clud-fix
description: Drive single GitHub issues or meta/parent burn-down issues until PRs are merged, issues are closed, and reported reproductions are validated fixed on main.
triggers:
  - When the user says "/clud-fix <issue-url-or-num>"
  - When the user says "$clud-fix <issue-url-or-num>" from Codex
  - When the user asks to fix a GitHub issue and expects merged-and-validated, not just a PR
  - When the user points at a meta, parent, tracking, epic, or burn-down issue and asks for all sub-issues fixed
  - When the user invokes "/goal $clud-fix <issue-or-issue-url>"
---
<!-- managed-by: clud -->

# /clud-fix

Drive a GitHub issue until the real issue-level completion condition is true.
For a single issue, that means PR(s) merged to the default branch, the issue
closed, and the reported reproduction or acceptance check validated on current
main. For a meta/parent/burn-down issue, that means every child issue is closed
and validated, the parent checklist is updated where possible, and the parent
issue itself is closed.

This skill is the outer issue orchestrator for both Claude and Codex through
clud. It delegates one-PR work and PR merge work to [[clud-pr]], but it owns the
issue-level `/goal` lifecycle. Do not invoke or depend on a standalone merge
skill.

For code changes, preserve RED -> GREEN: identify or add the focused failing
test or executable reproduction first, implement the scoped fix, then rerun that
focused signal until it passes before broad gates.

## Input

- A GitHub issue URL such as `https://github.com/<owner>/<repo>/issues/<num>`.
- A GitHub PR URL such as `https://github.com/<owner>/<repo>/pull/<num>`.
- A bare `#<num>` (issue) or `!<num>` (PR) only when the current checkout is the
  right repository. Resolve `<owner>/<repo>` with `gh repo view` before acting.

The argument can be a single issue, a meta/parent/burn-down issue that lists
sub-issues, or a PR. Classify the mode before planning any implementation.

### PR URL input

A PR URL input bypasses the single-issue / meta classifier entirely and
delegates to [[clud-pr]] PR Drive Mode: actively check CI builders + code-review
comments (CodeRabbit, GitHub Copilot review, human reviewers) + files needing
resolve, apply scoped fixes, push the new commits without force-push, and merge
when the PR is clean. Once [[clud-pr]] returns with the PR merged, this skill
validates against the underlying issue (if linked via `Closes #<N>`) and closes
it if validation passes.

Recognition rules for distinguishing PR URLs vs issue URLs vs bare numbers are
documented under "Input Recognition" in [[clud-pr]] — same disambiguation logic
applies here. See #395 for the spec.

This makes `/clud-fix` symmetric: issue URL → "fix this issue end-to-end";
PR URL → "drive this PR end-to-end through merge + validation."

## Goal Ownership

When the user invokes `/goal $clud-fix <issue-or-issue-url>`, this skill owns
the outer goal until the issue-level completion condition is proven.

For a single issue, use a goal equivalent to:

```text
/goal Fix issue #N: PR(s) merged to main, issue closed, and reported reproduction validated fixed on current main.
```

For a meta/parent/burn-down issue, use a goal equivalent to:

```text
/goal Complete meta issue #N: every child issue closed/validated, parent checklist updated, parent issue closed, and final evidence reported.
```

When delegating to [[clud-pr]], explicitly tell it that this is a delegated
`clud-fix` call. Delegated `clud-pr` work must not invoke a nested `/goal`,
replace the outer goal, or narrow success to "PR opened." It must return
structured evidence for `clud-fix` to evaluate: issue URL, PR URL(s), PR merged
state, tests run, validation notes, blocker reason if any, and issue closure
state.

### Terminal sentinels for no-PR outcomes

Three legitimate `/clud-fix` runs reach an honest no-PR terminal: a single-issue
intake-failed investigation report, a meta empty-children investigation report,
and an unreachable-scope refusal (the upfront gate added in step 2 of the
single-issue workflow). In each case, emit a structured sentinel as the FINAL
line of the user-facing response so an outer-`/goal` evaluator can recognize
the terminal:

| Terminal | Sentinel |
|---|---|
| Single intake-failed | `<clud-fix:terminal kind=investigation-report-posted reason=intake-failed url=<issue-url>>` |
| Meta empty-children | `<clud-fix:terminal kind=empty-children-report-posted reason=parent-roadmap-unfiled url=<parent-url>>` |
| Unreachable scope | `<clud-fix:terminal kind=unreachable-scope-refused reason=<closed-source-target\|read-only-repo\|...> url=<issue-url>>` |

The sentinel is plain transcript text — no harness affordance is required to
emit it. Recognition by `/goal` evaluators is a separate upstream change
tracked at #367.

## Done When

### Single Issue

All three conditions are mandatory:

1. Every fixing PR for the issue is merged into the repository default branch.
2. The issue is closed on GitHub.
3. The reported reproduction or acceptance check passes against current main.

Missing any one means the goal is not done.

### Meta / Parent / Burn-Down Issue

All conditions are mandatory:

1. Every enumerated child issue is closed on GitHub.
2. Every child has validation evidence from the child issue's reported
   reproduction or acceptance criteria, or a documented skipped/blocker state
   that was surfaced to the user.
3. Parent checklist items are checked when the parent has a checklist entry for
   the child.
4. The parent issue is closed on GitHub.
5. Final evidence is reported: child issue URLs, PR URLs, validation evidence,
   skipped/blocker reasons, and parent closure state.

## Single-Issue Workflow

1. **Intake gate.** Fetch the issue with:

   ```bash
   gh issue view <num> --repo <owner>/<repo> --json number,state,title,body,labels,comments,url
   ```

   If the issue is not open, stop and report the current state. Do not silently
   reopen or push.

2. **Reachable-scope gate.** This skill's terminal requires a merged PR. If the
   work needed to satisfy the issue cannot reach a merged PR in this repo, the
   run cannot terminate cleanly — refuse upfront rather than entering the
   intake-failure path mid-run (the original failure documented in #365).

   Check both:

   - **Repo push permission.** If the issue lives in a third-party repo:

     ```bash
     gh repo view <owner>/<repo> --json viewerPermission,isFork,parent
     ```

     If `viewerPermission` is `READ` or `TRIAGE` AND no fork is configured for
     this user (or the user has not opted into a fork-and-PR flow for this
     repo), the run cannot reach "PR merged on `<owner>/<repo>:main`". Refuse
     with: "Issue lives at `<owner>/<repo>` where viewerPermission is
     `<level>`. `/clud-fix` cannot satisfy its 'PR merged' terminal here.
     Open an issue or fork-and-PR via [[clud-pr]] instead."

   - **Non-source-artifact heuristic.** Scan the issue body and title for
     phrases that imply the fix lives outside the user's source tree:

     - `@anthropic-ai/claude-code`, `claude-code harness`, `harness binary`,
       `compiled into the harness`
     - `closed-source`, `closed source`, `proprietary binary`,
       `vendor binary`, `vendored library`
     - explicit references to third-party install paths
       (`/usr/lib/<vendor>/`, `node_modules/@<vendor>/`, etc.) being the
       sole source of the bug

     Hitting the heuristic is not proof — it is a stop-and-confirm prompt:
     "Issue body suggests the fix targets `<artifact>`, which `/clud-fix`
     cannot reach (no source tree to modify, no PR target). Refuse the run
     unless the user confirms a reachable substitute scope (e.g. a clud-side
     workaround, a runtime guard, or filing an upstream issue)."

   When either branch trips and the user does not provide a reachable
   substitute, emit the unreachable-scope terminal sentinel as the FINAL
   line of the user-facing response:

   ```text
   <clud-fix:terminal kind=unreachable-scope-refused reason=<read-only-repo|closed-source-target|...> url=<issue-url>>
   ```

   Then stop. Do NOT widen scope to manufacture a PR-able task that
   satisfies the literal goal text but misses what the user actually asked
   for; that pattern is exactly what made the `/goal` Stop-hook loop in
   #365 wedge.

3. **Readiness check.** A fixable single issue needs:
   - concrete reproduction: command, code snippet, or observable behavior
   - falsifiable symptom: wrong output, error, stack trace, screenshot, or
     specific misbehavior
   - fixed criterion: expected behavior or acceptance checklist
   - bounded scope: a bug fix or small feature, not open-ended design

4. **Under-specified issues get investigation first.** If any readiness item is
   missing, post an investigation report to the issue instead of opening a PR.

   Lead the report with a hook-mismatch banner (Markdown blockquote, first
   line) so an outer-`/goal`-following reader sees the no-PR terminal
   explicitly:

   ```markdown
   > ⚠ **/clud-fix intake gate failed.** This comment is the deliverable, not
   > a merged PR. If a session `/goal` hook references this issue URL, run
   > `/goal clear` after reading — it will not auto-satisfy.
   ```

   Then include root-cause evidence, reproduction status, planned fix,
   validation command, and open questions.

   In the final user response, emit the terminal sentinel as the last line:

   ```text
   <clud-fix:terminal kind=investigation-report-posted reason=intake-failed url=<issue-url>>
   ```

   Then stop with the blocker surfaced; do not manufacture a PR that cannot be
   validated.

5. **Survey existing PRs.** Search for PRs that mention or close the issue:

   ```bash
   gh pr list --repo <owner>/<repo> --search "<num> in:body" --state all --json number,state,headRefName,mergedAt,url
   ```

   If a merged PR already claims the issue, validate against current main and
   close the issue if validation succeeds. If an open PR exists, delegate to
   [[clud-pr]] PR merge mode, in delegated mode, to drive it through CI/review
   fixes and merge.

6. **Plan validation before implementation.** Identify files to touch, focused
   RED test/repro, and the post-merge validation command. For non-trivial design
   scope, check in with the user before opening a PR.

7. **Delegate PR work to [[clud-pr]].** Hand off the issue URL and state:
   "delegated from `clud-fix`; do not set a nested `/goal`; return structured
   evidence." [[clud-pr]] owns the disposable worktree, RED -> GREEN cycle,
   lint/test gates, PR creation, CI/review fix loop, and merge mode.

8. **Validate on main.** After merge, run:

   ```bash
   git fetch origin && git checkout main && git pull
   ```

   Then run the issue's original reproduction or acceptance command. CI green is
   not enough; validation must exercise the reported behavior.

9. **Close or confirm closed.** Verify with:

   ```bash
   gh issue view <num> --repo <owner>/<repo> --json state,closedAt
   ```

   If the issue is still open after a validated merge, close it with a comment
   naming the merged PR and validation evidence.

10. **Report.** Return the merged PR URL(s), the closed issue URL, and validation
   evidence.

## Meta / Parent / Burn-Down Workflow

1. **Fetch and classify the parent.** Treat an issue as meta/parent/burn-down
   when labels, title, checklist body, linked issue references, GitHub sub-issue
   metadata, or explicit user wording identify a tracker.

2. **Enumerate child issues.** Take the **union** of every issue reference
   appearing **anywhere** in the parent body, regardless of the surrounding
   markdown structure (checklist, table cell, prose paragraph, bullet list,
   heading, blockquote). All of the following patterns count, and any single
   match makes that issue a child candidate:

   - markdown checklist lines like `- [ ] #123` or `- [x] #124`
   - bare `#123` references in tables, prose, or any other markdown structure
     (e.g. a `| **#3237** | ... |` table cell, or `see #123` inline)
   - `owner/repo#123` (e.g. `FastLED/fbuild#627`) and bare `<repo>#123` when
     the parent body or repo context disambiguates the owner
   - full GitHub issue URLs in any position
   - GitHub sub-issue metadata when available

   Do **not** require an issue reference to be inside a `- [ ] #N` checklist to
   count it. Roadmap metas frequently catalog work in tables; treat each
   tabled `#N` as an enumerated child. If a reference is cross-repo or
   ambiguous (e.g. `fbuild#627` with no owner prefix), record it in the ledger
   with the resolved `owner/repo` you used and surface the resolution choice
   in the status line so the user can correct it.

   For each enumerated candidate, classify before queuing:

   - **Verify state on GitHub** before deciding it is already done — a checked
     `- [x] #N` is not authoritative; fetch the issue's `state`.
   - **Open** children are queued in listed order.
   - **Closed** children are skipped with a one-line ledger entry.
   - **Missing/inaccessible** children (404, private, wrong repo) are recorded
     as `blocked` with the resolution attempt noted, then surfaced.

   If after this union enumeration the open-children list is still empty (the
   parent is a roadmap whose child items have not been filed as issues yet, or
   every reference resolves to a closed/missing issue), do NOT proceed to
   ledger creation. Post the same investigation-report pattern as the
   single-issue under-specified branch — including the hook-mismatch banner —
   to the parent issue, listing every reference you found and its resolved
   state, and explaining which child issues need to be filed to unblock the
   burn-down. Emit the terminal sentinel as the last line of the user-facing
   response:

   ```text
   <clud-fix:terminal kind=empty-children-report-posted reason=parent-roadmap-unfiled url=<parent-url>>
   ```

   Then stop. Do NOT auto-file phase items to extend the loop; see the
   "Manufacturing new child issues" failure mode.

3. **Create/update the durable ledger.** Store progress in:

   ```text
   .clud/fix/<owner>__<repo>__issue-<num>.json
   ```

   Track parent issue URL/state, enumerated children, child status
   (`pending`, `in_progress`, `closed`, `skipped`, `blocked`), PR URLs,
   validation evidence, parent checklist update state, and final parent closure
   evidence. Use the ledger to resume after context limits, process exits, or
   manual interruptions.

4. **Process children sequentially.** Work one open child at a time, in listed
   order. Never parallelize child issues; parallel PRs make CI and status
   reporting incoherent.

5. **Run the single-issue workflow for each child.** Re-enter the single-issue
   workflow from intake through validation and closure. Under-specified children
   get an investigation report and are marked `skipped` or `blocked` in the
   ledger; they are not retried indefinitely in the same run.

6. **Update the parent after each child.** When a child is closed and validated,
   tick the parent checklist item if present. Post a concise status line:
   `[meta #N] child #X -> CLOSED (validated by <command/evidence>)` or
   `[meta #N] child #X -> skipped (<reason>)`.

7. **Refresh the parent between children.** Re-fetch the parent body and
   metadata so newly added child issues are picked up and externally closed
   children are skipped.

8. **Close the parent only when complete.** When the refreshed child list has no
   open children and no unresolved blockers, close the parent issue with a
   summary of child outcomes and validation evidence.

9. **Final report.** Report parent URL and CLOSED state, child issue URLs,
   merged PR URLs, validation evidence, and any skipped/blocker details.

## Loop Caps

- Single issue: at most 10 PRs total, 3 merge/fix rounds per PR, 2 validation
  retries.
- Meta/parent issue: no outer child-count cap. Per-child caps still apply. If a
  child exhausts a cap, mark it blocked/skipped in the ledger, surface it, and
  continue to the next child unless the user asks to stop.

## Claude And Codex Parity

This skill must work the same through clud for Claude and Codex:

- Codex reads the bundled skill from `~/.codex/skills/clud-fix/SKILL.md`.
- Claude reads the managed skill from `~/.claude/skills/clud-fix/SKILL.md`.
- The bundled source in `crates/clud-bin/assets/skills/clud-fix/SKILL.md` is the
  canonical workflow for both backends.
- Stale Claude-local managed copies must be updated by clud's Claude drift
  installer; stale standalone merge skill installs must be purged safely.

## Failure Modes To Avoid

- Letting delegated [[clud-pr]] replace the outer `clud-fix` `/goal`.
- Calling the goal done after PR merge alone.
- Closing a parent issue while any child issue is still open or unvalidated.
- Skipping validation because CI passed.
- Opening a PR for an issue that lacks a concrete reproduction or fixed
  criterion.
- Working meta child issues in parallel.
- Depending on a standalone merge skill; use [[clud-pr]] PR merge mode.
- Treating a checked checklist item as proof without verifying the child issue
  state on GitHub.
- Manufacturing new child issues after the open-child list is empty.
- Treating a user-installed outer `/goal /clud-fix <url>` wrap as authoritative.
  This skill owns its `/goal` lifecycle (see Goal Ownership). An outer wrap
  installs a parallel hook whose literal text the evaluator reads as a strict
  "PR merged" terminal; the evaluator cannot see this skill's no-PR terminals
  (intake-failed report, empty-children report, unreachable-scope refusal). On
  detecting an outer wrap that conflicts with a legitimate no-PR terminal,
  surface the conflict in the final user response and recommend `/goal clear`.
  Do not widen scope to satisfy the outer hook.

## When Not To Use This

- The user only wants a PR opened and does not require merge, closure, and
  validation; use [[clud-pr]].
- The task is design discussion rather than a bounded issue fix.
- The requested issue has no falsifiable reproduction and no acceptance
  criteria; post an investigation report first.
- Wrapping `/clud-fix` in an outer `/goal` (e.g. `/goal /clud-fix <url>`). Just
  invoke `/clud-fix <url>` directly. The skill owns its own `/goal` lifecycle
  (Goal Ownership). Wrapping creates a parallel hook the skill cannot see,
  which loops indefinitely when intake fails, children are empty, or scope is
  unreachable. The skill's no-PR terminals emit a sentinel intended for the
  inner goal to recognize; an outer wrap installed by the user cannot see it.
