---
name: git-workflow
description: Git branching, commit, and PR best practices
triggers:
  - When creating commits
  - When creating branches
  - When preparing pull requests
---

# Git Workflow

## Branch Naming

```
feat/short-description    — new features
fix/short-description     — bug fixes
refactor/short-description — code restructuring
docs/short-description    — documentation only
test/short-description    — test additions/fixes
chore/short-description   — maintenance, deps, CI
```

## Commit Messages

Use conventional commits format:
```
type(scope): concise description

- Why this change is needed (not what changed — the diff shows that)
- Any non-obvious decisions or tradeoffs
```

Types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`, `ci`

## Pull Requests

- Title: under 70 characters, matches the primary commit type
- Body: Summary (1-3 bullets), Test Plan (checklist)
- One logical change per PR — split unrelated changes
- Link to relevant issues

## Rules

- Never force-push to main/master
- Never commit secrets, credentials, or .env files
- Run tests before pushing
- Rebase on main before creating PR (avoid merge commits)
