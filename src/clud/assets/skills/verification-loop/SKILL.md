---
name: verification-loop
description: Run a 4-phase quality gate after implementation changes
triggers:
  - After completing a coding task
  - When asked to verify or validate changes
  - Before creating a pull request
---

# Verification Loop

Run this quality gate after any implementation work to catch issues early.

## Phases

### Phase 1: Build
Run the project's build command. If no build step exists, skip.
```
# Detect and run: make build, npm run build, cargo build, go build, etc.
```

### Phase 2: Lint & Type Check
Run linters and type checkers for the project's language.
```
# Python: ruff check + pyright
# TypeScript: eslint + tsc --noEmit
# Go: golangci-lint
# Rust: cargo clippy
```

### Phase 3: Test
Run the test suite. Focus on tests related to changed files.
```
# Run: pytest, npm test, go test, cargo test, etc.
# If full suite is slow, run only affected tests first
```

### Phase 4: Diff Review
Review your own changes before declaring done.
```
git diff --stat
git diff
```
- Check for: debug prints, TODO comments, hardcoded values, missing error handling
- Verify all changed files are intentional

## Rules

- Stop at the first failing phase — fix before continuing
- Report results for each phase (pass/fail with details)
- If all 4 phases pass, the change is ready for review
