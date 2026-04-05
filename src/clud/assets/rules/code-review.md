---
name: code-review
description: Code review triggers and quality standards
---

# Code Review Rules

## Mandatory Review Triggers

Automatically request code review when:
- Changing authentication or authorization logic
- Modifying database schemas or migrations
- Updating security-sensitive configuration
- Changing CI/CD pipelines
- Modifying public APIs

## Review Checklist

- [ ] Changes match the stated intent (no scope creep)
- [ ] Error handling is explicit and informative
- [ ] Tests cover new functionality and edge cases
- [ ] No hardcoded values that should be configurable
- [ ] No debug code or commented-out blocks left behind
- [ ] Breaking changes are documented

## Severity Classification

- **CRITICAL**: Security vulnerability, data loss, crash — block merge
- **HIGH**: User-facing bug, missing validation — should fix before merge
- **MEDIUM**: Code smell, missing test, unclear naming — fix soon
- **LOW**: Style preference, minor optimization — optional
