---
name: grind-intake
description: "Turn a /grind argument (meta issue URL, issue numbers or URLs, free-form goal, or nothing for the repo's issues page) into an ordered goal list."
triggers:
  - When /grind needs its goal list
  - When the user asks which goals a meta issue or issue list expands to
---
<!-- managed-by: clud -->

# /grind-intake

Produces the ordered goal list the `/grind` router passes to the workflow:
`[{id, title, brief}]`, plus the repository root and default branch. It
changes no code, so the RED -> GREEN rule applies later, in
`/grind-integrate`.

## Sources

Recognize the argument in this order:

1. **Meta issue URL or number.** A parent issue with native sub-issues
   (`gh api repos/{owner}/{repo}/issues/{n}/sub_issues`) or a task list of
   issue references. Each open child becomes a goal; the parent is closed by
   `/grind` after every child resolves, never by a child's PR.
2. **Issue list.** Numbers or URLs separated by spaces or commas. Each open
   issue becomes a goal.
3. **Free-form goal.** Text that is not an issue reference. Split it into
   independent deliverables; one cohesive change stays one goal.
4. **Nothing.** List the repo's open issues (`gh issue list`), excluding
   ones with an open linked PR, and ask the user which to take.

## Each goal

- `id`: the issue number, or `g1`, `g2`, … for free-form goals.
- `title`: the issue title or a short name.
- `brief`: the issue URL (agents read it themselves) or a self-contained
  description of the deliverable with its acceptance criteria.

Order goals so that anything another goal builds on comes first; the
planner makes the dependencies explicit.

Resolve the repository root with `git rev-parse --show-toplevel` and the
default branch from `origin/HEAD` (`main` or `master`).
