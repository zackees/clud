---
name: grind-plan
description: "Plan one /grind goal: pick its checkout, split it into worker tasks with disjoint files, declare dependencies on other goals, and name the integrator's verify commands. Read-only; never builds."
triggers:
  - When the grind workflow's planner starts a goal
  - When the user asks to plan a goal into disjoint worker tasks
disable-model-invocation: true
---
<!-- managed-by: clud -->

# /grind-plan

You plan one goal; you never build, lint or test. The integrator does that
later, one goal at a time.

1. **Read the goal.** If the brief is an issue, `gh issue view <n>
   --comments`. Read the code it names and the repo's per-directory READMEs.
2. **Checkout and branch.** Branch `grind/<id>-<slug>`. `<base>` is the
   prompt's "Base branch": `<main>` for a bug-stage goal, the feature branch
   for a feature-stage goal.
   - Parallel mode: `git -C <repo> fetch origin <base>`, then
     `git -C <repo> worktree add <repo>-wt-<id> -b <branch> origin/<base>`
     (reuse it if it exists). The checkout is that worktree.
   - Sequential mode: the checkout is the repository itself. Do not create a
     worktree or switch branches; the integrator creates the branch.
3. **Tasks.** One to eight tasks, each with a **disjoint** file set: two
   tasks never write the same file. Each task's instructions must be
   complete for a worker that cannot run anything: exact paths, what to
   change, what done looks like, which existing code to mirror, and the
   RED -> GREEN regression test to write for a code change.
   Never plan a task that only runs commands ("verify build and tests",
   "run clippy"): workers and reviewers cannot run anything, so it can only
   end "Not done". Every task writes at least one file; checks go in step 5.
4. **Dependencies.** `depends_on` lists other goals in this run that must
   land first (shared files, an API this goal consumes). Only goals listed
   before this one count; the workflow ignores any other. An empty list
   means the goal is isolated and rebases straight onto `origin/<base>`.
   Keep chains short; a dependent goal waits for its dependency to merge.
5. **Verify.** The exact lint, build and test commands the integrator runs,
   newest-first focused test before the broad gates, taken from the repo's
   own docs (for example `bash lint`, `bash test`). Name the `ci.yml` job
   to run under `act` only if local CI is on. When the prompt says the run
   has scripts (`./lint`, `./test`), the integrator already runs them before
   every push: do not invent lint or test commands; verify is just the
   focused test.

## Plan-only mode

When the prompt says PLAN-ONLY, skip steps 2-5 and only classify:

- Read each child (`gh issue view <n> --comments`) and the code it names.
- Classify each child as a **bug** (a self-contained fix to existing
  behaviour, independent of the other children, landing as its own PR into
  the default branch) or a **feature** (one part of a single cohesive
  change).
- Name the feature groups and say whether each is independent of the others.
- List which feature children depend on which bugs (`depends_on_bugs`), and
  give a dependency order for all children.
- Return every child exactly once, with its id as the prompt gives it.
- Set `confident` to false if any child cannot be placed or the split is a
  guess.

Never write files, create worktrees or branches, or push; clud's hook refuses
them while `run.json` has `"phase": "plan"`. Return the classification
through StructuredOutput.

Otherwise, return the plan through StructuredOutput.
