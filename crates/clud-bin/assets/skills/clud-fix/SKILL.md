---
name: clud-fix
description: Drive a GitHub issue end-to-end until PRs are merged, the issue is closed, and the reported reproduction is validated fixed on main.
triggers:
  - When the user says "/clud-fix <issue-url-or-num>"
  - When the user asks to fix a GitHub issue and expects merged-and-validated, not just a PR
  - When a previous /clud-pr run closed but the issue is still open or unvalidated
  - When the user points at a meta, tracking, or epic issue and asks for all sub-issues fixed
---
<!-- managed-by: clud -->

# /clud-fix

Drive a GitHub issue until all required closure evidence exists: the fixing PR
is merged to the default branch, the issue is closed, and the issue's reported
reproduction no longer reproduces on current main. This skill orchestrates
existing clud workflows; it does not reimplement the worktree, CI, review, or
merge logic owned by [[clud-pr]].

For code changes, preserve RED -> GREEN: identify or add the focused failing
test or executable reproduction first, implement the scoped fix, then rerun that
focused signal until it passes before broad gates.

## Input

- A GitHub issue URL such as `https://github.com/<owner>/<repo>/issues/<num>`.
- A bare `#<num>` only when the current checkout is the right repository. Resolve
  `<owner>/<repo>` with `gh repo view` before acting.

The argument can be a single issue or a meta/tracking issue that lists
sub-issues. Classify the mode before planning any implementation.

## Done When

For a single issue, all three conditions are mandatory:

1. Every PR opened for the issue is merged into the repository default branch.
2. The issue is closed on GitHub.
3. The reported reproduction or acceptance check passes against current main.

For a meta issue, process open sub-issues sequentially until the open sub-issue
list is empty, the meta issue closes, the user stops the run, or an explicit
blocker/cap is reached.

## Workflow

1. **Intake gate.** Fetch the issue with
   `gh issue view <num> --repo <owner>/<repo> --json number,state,title,body,labels,comments,url`.
   If the issue is not open, stop and report the current state.
2. **Classify single vs. meta.** Treat it as meta when labels, title, body, or
   the user's invocation clearly identify a tracking/epic issue or a checklist
   of child issue references.
3. **For under-specified single issues, investigate first.** If there is no
   concrete reproduction, falsifiable symptom, fixed criterion, or bounded
   scope, post an investigation report to the issue instead of opening a PR.
   Include root-cause evidence, reproduction status, planned fix, validation
   command, and open questions.
4. **Survey existing PRs.** Search for PRs that mention or close the issue. If a
   merged PR already claims the issue, validate against current main and close
   the issue if validation succeeds. If an open PR exists, use [[clud-pr]] PR
   merge mode to drive it through CI/review fixes and merge.
5. **Plan the fix.** Identify files to touch, tests to add, and the validation
   command that proves the reported reproduction is fixed. For non-trivial
   design scope, check in with the user before opening a PR.
6. **Implement and ship via [[clud-pr]].** Hand off the issue URL or PR URL to
   [[clud-pr]]. It owns the disposable worktree, focused RED -> GREEN cycle,
   broad lint/test gates, commit, push, PR creation, CI/review fix loop, and
   merge mode.
7. **Validate on main.** After merge, run
   `git fetch origin && git checkout main && git pull` in the main checkout, then
   run the issue's original reproduction or acceptance command. CI green is not
   enough; validation must exercise the reported behavior.
8. **Close or confirm closed.** Verify with `gh issue view <num> --json state`.
   If the issue is still open after a validated merge, close it with a comment
   naming the merged PR and validation evidence.
9. **Report.** Return the merged PR URL(s), the closed issue URL, and one
   sentence of validation evidence.

## Meta Workflow

1. Enumerate child issue references from the meta issue body and GitHub issue
   metadata.
2. Work one open child issue at a time, in listed order. Do not parallelize
   sub-issues.
3. For each child, run the single-issue workflow from intake through validation
   and closure.
4. Refresh the child list between iterations so newly closed or newly added
   issues are handled correctly.
5. When no open child issues remain, report the per-child outcomes and stop. Do
   not manufacture new work.

## Failure Modes To Avoid

- Treating PR merge as sufficient without issue closure and reproduction
  validation.
- Opening a PR for an issue that lacks a concrete reproduction or fixed
  criterion.
- Skipping validation because CI passed.
- Expecting a standalone merge skill; use [[clud-pr]] PR merge mode.
- Working meta sub-issues in parallel.
- Pushing to an unfamiliar third-party repository without confirming access and
  intent with the user.

## When Not To Use This

- The user only wants a PR opened and does not require merge, closure, and
  validation; use [[clud-pr]].
- The task is design discussion rather than a bounded issue fix.
- The requested issue has no falsifiable reproduction and no acceptance
  criteria; post an investigation report first.
