---
name: grind-plan
description: "Plan one /grind goal: pick its checkout, split it into worker tasks with disjoint files, declare dependencies on other goals, and name the integrator's verify commands. Read-only; never builds."
triggers:
  - When the grind workflow's planner starts a goal
  - When the user asks to plan a goal into disjoint worker tasks
---
<!-- managed-by: clud -->

# /grind-plan

You plan one goal; you never build, lint or test. The integrator does that
later, one goal at a time.

1. **Read the goal.** If the brief is an issue, `gh issue view <n>
   --comments`. Read the code it names and the repo's per-directory READMEs.
2. **Checkout and branch.** Branch `grind/<id>-<slug>`.
   - Parallel mode: `git -C <repo> fetch origin <main>`, then
     `git -C <repo> worktree add <repo>-wt-<id> -b <branch> origin/<main>`
     (reuse it if it exists). The checkout is that worktree.
   - Sequential mode: the checkout is the repository itself. Do not create a
     worktree or switch branches; the integrator creates the branch.
3. **Tasks.** One to eight tasks, each with a **disjoint** file set: two
   tasks never write the same file. Each task's instructions must be
   complete for a worker that cannot run anything: exact paths, what to
   change, what done looks like, which existing code to mirror, and the
   RED -> GREEN regression test to write for a code change.
4. **Dependencies.** `depends_on` lists other goals in this run that must
   land first (shared files, an API this goal consumes). Only goals listed
   before this one count; the workflow ignores any other. An empty list
   means the goal is isolated and rebases straight onto `origin/<main>`.
   Keep chains short; a dependent goal waits for its dependency to merge.
5. **Verify.** The exact lint, build and test commands the integrator runs,
   newest-first focused test before the broad gates, taken from the repo's
   own docs (for example `bash lint`, `bash test`). Name the `ci.yml` job
   to run under `act` only if local CI is on.

Return the plan through StructuredOutput.
