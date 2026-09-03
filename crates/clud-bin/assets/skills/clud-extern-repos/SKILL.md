---
name: clud-extern-repos
description: Coordinate dependent cross-repo changes using a sister checkout directory beside the repo.
triggers:
  - When a task needs a dependent change in another repository
  - When the user asks to use extern repos or sister checkouts for cross-repo work
---
<!-- managed-by: clud -->

# clud-extern-repos

Use this workflow when the current repo needs coordinated work in a dependent repository.

## Code Change Rule

If any parent or dependent repository change is a bug fix or feature implementation, use RED -> GREEN in that repository: write or identify the focused failing test/repro first, run it to fail, implement the scoped change, rerun it to pass, then run the broader repo checks before opening linked PRs.

## Convention

Place each dependent checkout **beside** the repo, not inside it:

```
~/dev/myrepo            <- the repo you are working in
~/dev/myrepo-extern/    <- dependent checkouts go here
    running-process/
```

So from `~/dev/myrepo`, clone to `../myrepo-extern/<repo-name>`. Only immediate children of that directory are tracked by clud GC.

The directory is derived from the repo's own name, and for a worktree it derives from the **main** repo — `~/dev/myrepo-wt-123` also uses `~/dev/myrepo-extern`, so a dependency is cloned once rather than once per worktree.

You do not need to add an ignore entry for it, and you should not: it is outside the repo, so nothing that walks the repo can reach it. That is the point. A checkout inside the repo has to be excluded by every linter, formatter, test collector, and build script pointed at the root, and a wrong exclusion fails silently — `ci/banned_imports.py` in clud carried one for its whole life without anybody noticing.

Checkouts still under the older `<repo>/.extern-repos/` location keep working and are still tracked, but put new ones beside the repo.

A dependent checkout is a **complete project**, so parent-repo tooling that
discovers projects by walking up from `$PWD` will find it and bind to it. That
is not hypothetical: in #972 a Stop hook installed in the parent repo resolved
its root that way, landed in the dependent checkout, and ran *that* project's
hook through `uv run` — turning a "quick lint" into a full native build of a
Rust-backed dependency. Two fires cost ~600s and ~400s, and the second died
with a build-backend error that named no hook at all.

Two rules follow, and they apply wherever the checkout lives:

- **Anchor hooks to the session project root**, not `$PWD`. Claude Code passes
  it in the hook payload (`cwd`), and the settings live in that repo anyway. A
  hook installed in repo A should never execute repo B's hook.
- **Never let a hook trigger a project sync**: use `uv run --no-project` (or
  `--frozen`). A hook may lint; it may not compile a dependency from source.
  clud's `uv_run_hook_guard` warns about the bare form, and since #972 it looks
  at `Stop` hooks and at dependent checkouts in both locations.

Create feature branches in the dependent repo using the `feat/<short-name>` convention.

## Coordination

Keep each repo's work scoped to that repo. Do not edit the parent repo from inside the dependent checkout, and do not edit the dependent checkout from the parent repo.

When opening PRs, link both directions:

- Parent PR body: `Depends on <owner>/<repo>#<number>`
- Dependent PR body: `Coordinated with <owner>/<parent-repo>#<number>`

If the dependent PR must land first, make that ordering explicit in the parent PR body.

## Cleanup

The daemon auto-removes `.extern-repos/<name>/` once the directory has been inactive (no descendant `mtime` change) for at least 24 hours and no live clud session is rooted inside it. Anything tracked under `.extern-repos/` is clud-managed by convention, so don't park work there that you want kept indefinitely — copy it elsewhere or commit + push it first.

Override the inactivity window via `CLUD_GC_EXTERN_REPO_MAX_AGE_SECS` (default `86400`).
