---
name: continuous-learning
description: Extract reusable patterns and lessons from coding sessions
triggers:
  - At the end of a coding session
  - After resolving a difficult bug
  - After a successful refactoring
---

# Continuous Learning

After completing significant work, extract patterns that could help in future sessions.

## What to Capture

1. **Project-specific patterns** — Build commands, test patterns, deployment steps
2. **Codebase conventions** — Naming patterns, file organization, API styles
3. **Gotchas** — Non-obvious behaviors, common pitfalls, environment quirks
4. **Decisions** — Why a particular approach was chosen over alternatives

## Capture Format

Save learned patterns to the project's CLAUDE.md or memory system:

```markdown
## Learned: [Pattern Name]
**Context:** [When this applies]
**Pattern:** [What to do]
**Why:** [Reasoning or prior incident]
```

## What NOT to Capture

- Obvious language features or stdlib usage
- One-time debugging steps
- Information already in the README or docs
- Ephemeral state (current branch, in-progress work)

## Rules

- Only capture patterns you're confident about (used successfully 2+ times)
- Keep entries concise — one paragraph max
- Update existing entries rather than creating duplicates
- Remove entries that turn out to be wrong
