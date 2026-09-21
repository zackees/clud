---
name: clud-meta-work
description: "Orchestrate a /goal or clud do request with several independent deliverables: plan disjoint work, delegate safely, review, and integrate under the repository's own checks."
triggers:
  - When clud do or /goal explicitly asks for /clud-meta-work
  - When one requested outcome contains multiple independent deliverables that can be worked in parallel
  - When the user asks for a meta-work style orchestration run
---
<!-- managed-by: clud -->

# /clud-meta-work

Use this playbook only after confirming that the requested result has at least
two genuinely independent deliverables. A single cohesive change stays in the
normal `/goal` flow; splitting it creates merge conflicts and obscures
ownership.

## Workflow

1. **Plan first.** Read the repository instructions and the target. Break it
   into small slices with disjoint file ownership, clear acceptance criteria,
   and a named validation command. Keep shared refactors, release work, and
   integration-sensitive changes in one serial slice.
2. **Delegate only independent slices.** Give each worker its exact files,
   acceptance criteria, and instruction to avoid unrelated edits. Use isolated
   worktrees only when the repository's own worktree guidance permits them;
   otherwise coordinate serially in the current checkout. Never assume a
   machine-local tool, agent name, model name, admin privilege, or merge policy.
3. **Review before integration.** Inspect every worker diff and resolve
   conflicts deliberately. Run the repository-required review step for source
   changes. A worker result is evidence, not approval to merge.
4. **Integrate one slice at a time.** Apply or merge the reviewed changes in a
   deterministic order, run focused checks after each risky integration, then
   run the repository's required lint and test commands for the combined
   result. Do not use blanket `git add -A` or discard unrelated user changes.
5. **Land according to repository policy.** Create, validate, and merge PRs
   only when the target repository permits it. Wait for required CI; never use
   an administrator bypass merely because this playbook is running. Close the
   goal only after the requested work is actually landed and the checkout is
   clean.

## Model roles on Codex via Claude

Read `CLUD_ROUTE_CONTEXT` before creating an agent. It is JSON written by clud
for this child process and is the authority for `model_provider`, `harness`,
and delegation policy. Do not infer the route from the visible Claude model
name, `$TERM`, installed binaries, or an inherited user value.

When its `model_provider` is `codex` and `harness` is `claude`, choose `opus`
for planning, review, and integration and `sonnet` for ordinary implementation.
clud maps those role aliases to Codex Sol and Codex Terra, respectively. This
keeps many implementation workers on the more affordable tier while preserving
one stronger tier for the small number of orchestration decisions. Do not
hard-code raw provider IDs: the route owns that mapping.

For every other route, use the harness's native model-selection mechanism and
the context's `cost_policy`: start workers on the cheapest capable tier and
escalate only when a task demonstrably needs it. Never borrow Claude aliases
for a native Codex session or Codex discovery IDs for a native Claude session.

## Guardrails

- Do not parallelize overlapping files or a change that needs a single mental
  model.
- Do not require `act`, `bosn`, a particular forge CLI, or a specific CI
  provider; discover the repository's documented workflow instead.
- Preserve user changes and never delete a worktree or branch until its state
  and ownership have been checked.
- If a slice cannot be validated independently, return it to the serial
  integration slice.
