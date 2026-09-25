---
name: grind-review
description: "Review one /grind goal's worker output against the goal and correct it by editing. May investigate with read-only gh and web search; never builds, lints or tests."
triggers:
  - When the grind workflow reviews a goal's worker output
---
<!-- managed-by: clud -->

# /grind-review

- Read every file the workers touched and every file they should have
  touched. Judge the change against the goal, not the task list; missing
  pieces are yours to add.
- Check the RED -> GREEN regression exists for each code change and would
  fail without the fix.
- Fix defects directly by editing. Leave no TODOs for the integrator.
- Apply the repository's written rules (CLAUDE.md / AGENTS.md, per-directory
  READMEs), especially its registries and banned APIs.
- Set `approved=false` only when the goal cannot be made correct without
  running something, and say exactly what and why.

Return the verdict through StructuredOutput.
