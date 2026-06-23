---
name: clud-extern-repos
description: Coordinate dependent cross-repo changes under a repo-local .extern-repos/ checkout convention.
triggers:
  - When a task needs a dependent change in another repository
  - When the user asks to use .extern-repos for cross-repo work
---
<!-- managed-by: clud -->

# clud-extern-repos

Use this workflow when the current repo needs coordinated work in a dependent repository.

## Code Change Rule

If any parent or dependent repository change is a bug fix or feature implementation, use RED -> GREEN in that repository: write or identify the focused failing test/repro first, run it to fail, implement the scoped change, rerun it to pass, then run the broader repo checks before opening linked PRs.

## Convention

Place each dependent checkout at `<current-repo>/.extern-repos/<repo-name>/`. Only immediate children of `.extern-repos/` are tracked by clud GC.

Before cloning, verify `.extern-repos/` is ignored by the current repo. If it is not ignored, ask before adding the ignore entry and refuse to create an unignored clone.

Create feature branches in the dependent repo with the same short-name style as clud-pr, for example `feat/<short-name>`.

## Coordination

Keep each repo's work scoped to that repo. Do not edit the parent repo from inside the dependent checkout, and do not edit the dependent checkout from the parent repo.

When opening PRs, link both directions:

- Parent PR body: `Depends on <owner>/<repo>#<number>`
- Dependent PR body: `Coordinated with <owner>/<parent-repo>#<number>`

If the dependent PR must land first, make that ordering explicit in the parent PR body.

## Cleanup

The daemon auto-removes `.extern-repos/<name>/` once the directory has been inactive (no descendant `mtime` change) for at least 24 hours and no live clud session is rooted inside it. Anything tracked under `.extern-repos/` is clud-managed by convention, so don't park work there that you want kept indefinitely — copy it elsewhere or commit + push it first.

Override the inactivity window via `CLUD_GC_EXTERN_REPO_MAX_AGE_SECS` (default `86400`).
