---
name: git-workflow
description: Git workflow rules and conventions
---

# Git Workflow Rules

## Branch Protection

- Never force-push to main/master
- Never commit directly to main — use feature branches
- Delete branches after merge

## Commit Standards

- Use conventional commit format: `type(scope): description`
- Keep commits atomic — one logical change per commit
- Write commit messages in imperative mood ("add feature" not "added feature")
- Reference issue numbers when applicable

## Merge Strategy

- Rebase feature branches on main before creating PR
- Squash-merge when the branch has messy intermediate commits
- Regular merge when all commits are clean and meaningful

## Forbidden Actions

- `git push --force` to shared branches
- `git reset --hard` on shared branches
- Committing files with merge conflict markers
- Committing generated files (build output, node_modules, __pycache__)
