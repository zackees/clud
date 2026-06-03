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

The daemon may auto-remove `.extern-repos/<name>/` only after the tracked branch has a merged PR, the directory has been inactive for at least 24 hours, and no live clud session is rooted inside it.

If `gh` is missing, rate-limited, or cannot confirm a merged PR, keep the checkout.
