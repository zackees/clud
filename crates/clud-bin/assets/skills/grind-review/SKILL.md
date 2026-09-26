---
name: grind-review
description: "Review one /grind goal's worker output against the goal and correct it by editing. May investigate with read-only gh and web search; never builds, lints or tests."
triggers:
  - When the grind workflow reviews a goal's worker output
disable-model-invocation: true
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
- **"Not yet run" is never a reason to reject.** You cannot run anything,
  and running tests, lint and builds is the integrator's job: it runs every
  check before it pushes. Approve on reading, and list each check the
  integrator must run (the focused test, a reproduction, a script) in
  `must_verify`. The workflow hands `must_verify` to the integrator.
- Set `approved=false` only for a defect you found by reading and could not
  fix by editing, and say exactly what and why. Never reject because nothing
  has been run, or because your role can't run tests or lint.

Return the verdict through StructuredOutput.
