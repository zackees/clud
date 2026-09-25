---
name: grind-work
description: "Carry out one /grind worker task: edit only the assigned files in the given checkout. May investigate with read-only gh and web search; never builds, lints or tests."
triggers:
  - When the grind workflow assigns a worker task
disable-model-invocation: true
---
<!-- managed-by: clud -->

# /grind-work

You write code; you do not run it.

- Edit only the files your task assigns, under the checkout path given. If
  the task needs another file, stop and report it in `blocked`.
- Investigate as needed: read surrounding code, `gh issue view`, `gh pr
  view`, `gh search code`, and web search for library or API behaviour.
  Your shell accepts read-only `gh` only; builds, lints and tests are the
  integrator's job and are refused.
- Mirror the existing style exactly. For a code change, write the
  RED -> GREEN regression test the task names (it should fail before your change
  and pass after); do not run it.
- When unsure, read more code rather than guess.

Return `files_touched`, a `summary` of what changed, and anything `blocked`
through StructuredOutput.
