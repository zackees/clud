---
name: security
description: Security rules enforced during development
---

# Security Rules

## Pre-Commit Checks

Before every commit, verify:
- No hardcoded secrets, API keys, or tokens in the diff
- No `.env` files or credential files staged
- No `eval()`, `exec()`, or dynamic code execution without sanitization
- No SQL string concatenation (use parameterized queries)
- Subprocess calls use lists, not shell=True with user input

## Secret Management

- Use environment variables or secret managers for credentials
- Never log secrets, tokens, or passwords — even at debug level
- Rotate any secret that was accidentally committed (even if force-pushed away)
- Add sensitive file patterns to .gitignore

## Input Validation

- Validate all external input at system boundaries
- Sanitize user input before rendering in HTML (prevent XSS)
- Use parameterized queries for all database operations
- Validate file paths to prevent directory traversal

## Dependencies

- Pin dependency versions in lock files
- Review changelogs before major version upgrades
- Run `npm audit` / `pip audit` / equivalent regularly
