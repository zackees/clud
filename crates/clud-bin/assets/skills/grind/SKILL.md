---
name: grind
description: "Work through a list of goals (a meta issue, issue list, free-form goal, or the repo's issues page) to merged PRs: plan, write, review, integrate and land, in parallel worktrees, sequentially in the local checkout, or one issue per /loop tick. Router for the grind-* skills and the bundled grind workflow."
triggers:
  - When the user runs /grind or clud grind
  - When clud do or /goal has several independent deliverables to implement
  - When the user asks to burn down a meta issue, an issue list, or the issues page
allowed-tools: Bash(clud grind-scripts:*)
---
<!-- managed-by: clud -->

!`clud grind-scripts`

The line above lists the repository's `./lint` and `./test` scripts and the
files they delegate to. If it still shows a command instead of that report
(the harness did not run it, or `clud` is missing), run `clud grind-scripts`
yourself from the repository root. If that fails too, treat the repository as
having no scripts and skip the scripts question.

# /grind

Router for the grind DAG. It gathers every answer up front, then hands the
deterministic part to the bundled `grind-run` workflow.

```
/grind ─ intake ─ classify + plan (plan-only) ─ preflight ─ ONE question round ─ prework ─┬─ parallel   ─┐
                                                                                          ├─ sequential ─┼─ workflow grind-run: plan → work → review → integrate → land ─ finish
                                                                                          └─ cron ─ /grind-cron ─ /loop: sequential run, one issue per tick
```

From the moment prework starts (the run's `grind-run` workflow, after the
question round), nobody asks: no agent and not the main session calls
AskUserQuestion, and clud's hook denies it for
every `grind-*` subagent. Anything unexpected follows a rule recorded in
advance in `run.json` (section 3).

Every code change keeps a RED -> GREEN focused regression: the integrator
first shows the failure or reproduction, then makes it pass before the
repository's broader gates.

## 0. Harness

`/grind` needs Claude Code's Workflow tool and the bundled `grind-*` agent
types. If either is missing (a Codex or other native harness), stop and say:
"/grind requires the Claude harness; relaunch with `--harness claude`." Do
not emulate the workflow by hand.

## 0b. Reconcile

At the start of every `/grind`, before intake, run `clud grind reconcile`
from the repository root and report each stale line it prints (an issue
whose goal PR landed on a feature branch but lacks the `grind:on-feature`
label, marker or feature PR `Closes` line, or a `grind/*` branch whose
commits survive only at `refs/pull/<n>/head`). A failure is reported, not
fatal. Optionally, mention once that a GitHub Action could run the same
reconcile on a schedule; setting one up is out of scope for this run.

## 1. Intake

Read `../grind-intake/SKILL.md`, next to this skill's base directory, and follow it with the user's argument (the leaf skills are hidden from the model, so they are read rather than invoked). It returns the goal list
(`[{id, title, brief}]`) in order, plus the `meta` issue, the repository root
and its default branch (`main` or `master`).

Intake always resolves the input to a meta issue: it creates or attaches one
for a prompt or issue list, or converts a multi-part issue after asking. That
conversion question (or, with no argument, the pick of which open issues to
take) is intake's only question and comes before section 1b.
Intake may instead end the run with a refusal ("run `/do N`"), "Nothing to
do", or a `gh` error; then stop there: skip every later section (Finish
included), write no `.clud/grind/run.json`, and create nothing.

## 1b. Plan before asking (classification)

After intake resolves the meta issue `T`, write `.clud/grind/run.json` at the
repository root as:

```json
{"phase": "plan"}
```

clud's command hook then denies the planner Write/Edit, `git worktree add`
and `git push`. Start the Workflow named `grind-run` with args
`{repo, main, meta, goals, planOnly: true}` and relay its output. The
Workflow tool returns at once and its result arrives later as a task
notification: end your turn and do nothing else until it arrives.

The classification is automatic: print one line per child, `#N bug → main`
or `#N feature → grind/meta-<T>-<group>`, and never ask about it.

The workflow also returns `path`:

- `simple` (the default) keeps the meta issue exactly as written: no new
  issues, no moved parents, no rewritten bodies. Print
  `keeping #T as is: <reason>`; there is no regroup question.
- `regroup` happens only when all of these hold: at least 2 independent
  feature groups of 3+ children each, 8+ children in total, and a confident
  classification with every child placed. Only then is the regroup plan
  shown in the question round.

**No overlap.** Before the question round, run
`gh pr list --state open --search 'head:grind/meta-<T>-' --json number,headRefName,isDraft`
(`T` is the top meta issue). If any open feature PR exists under the same
top meta `T`, the plan becomes bugs-only: drop every feature stage, set
`rules.no_overlap: "bugs_only"` and `"waiting_on_pr": <n>` in the plan, skip
the regroup and feature-merge questions, and report
`feature PR #<n> for #T is still open; this run is bugs-only`. A feature PR
under a different top meta never blocks; the scope is per top meta. Once
that PR is merged (or closed), the next run may pick a feature again.

Keep the per-child tracks (`bug` or `feature`) for section 3. Section 3 then
overwrites `run.json` without `phase`, lifting the plan-only caps.

## 1c. Repo-state preflight

Pick the run id now (4 lowercase hex characters); every later
`<run-id>` is this one. Then, read-only, from the repository root:

- `git status --porcelain -uall -- . ':(exclude).clud/grind'` (`-uall`
  lists untracked files one by one, so the exclusion applies to each)
- `git rev-parse --abbrev-ref HEAD` (record it as the starting branch)
- `git stash list`
- `git fetch origin`, then
  `git rev-list --left-right --count origin/<main>...HEAD` (behind, ahead).

`.clud/grind/` holds the run's own files (`run.json`, `plan.json`, the
feature worktree), never the user's changes: every preflight command here
excludes it with that pathspec, so it is never reported, stashed or
committed.

Repo state is asked about here and nowhere else. If the tree is dirty, the
question round includes the dirty-repo question (section 2). Apply the chosen
action right after the round, before prework:

- **Stash**: `git stash push -u -m grind-<run-id> -- . ':(exclude).clud/grind'`.
- **WIP branch**: `git switch -c wip/grind-<run-id>`, `git add -A -- .
  ':(exclude).clud/grind'`, `git commit -m "grind: WIP before run <run-id>"`,
  then `git switch <starting branch>`. Never push it.
- **Carry into the grind worktree**: offered only when the plan has a
  feature stage. Stash now, as
  `git stash push -u -m grind-<run-id>-carry -- . ':(exclude).clud/grind'`,
  so the user's checkout is clean; section 4b applies it in the feature
  worktree.
- **Abort**: stop. Create nothing on GitHub or in git, remove the
  plan-phase `.clud/grind/run.json`, and write no other file.

A clean tree records `preflight.action: "none"`.

## 2. One question round

At most 2 AskUserQuestion calls in total, and nothing is asked twice.
AskUserQuestion takes at most 4 questions per call and 2-4 options per
question (the user can always type another answer), so the round is fixed:

- **Call 1, the repo and the plan:** Dirty repo, Regroup, Mode, Models.
- **Call 2, the run's policies:** Local CI, Scripts, Feature merge policy,
  Problem reporting.

Skip an item whose condition does not hold. Problem reporting is always
asked, so a run that goes ahead makes exactly 2 calls; answering Abort in
call 1 stops before call 2. This is the only round: no question is asked
after it, by the main session or anyone else.

- **Dirty repo** (only if 1c found changes): show the file list; options
  Stash it, Commit to a WIP branch, Carry into the grind worktree (only when
  the plan has a feature stage), Abort.
- **Regroup** (only when 1b returned `regroup`): ONE single-select question
  that both confirms the regroup plan and picks exactly ONE feature group for
  this run: `Regroup; run <group> first` per group in dependency order (the
  first labelled "(Recommended)", at most 3), plus `Keep as is`. Every
  other group goes into the plan's `deferred_groups` as
  `{group, sub_meta, children}` with no branch assigned. `Keep as is` takes
  the `simple` path.
- **Mode.** (i) *Parallel*: each goal gets a git worktree; workers only read
  and write files; one integrator at a time rebases, lints, builds and tests.
  Uses GitHub's server-side concurrency. (ii) *Sequential*: one goal at a
  time from `origin/<main>` in the local checkout, no worktrees or sister
  clones; best for C++/Rust or other heavy repos where a cold worktree build
  is expensive. (iii) *Cron*: one issue per `/loop` tick, each tick a
  sequential run.
- **Models**, one question for the planner, worker, reviewer and integrator
  (the lander shares the integrator's, prework the planner's): first "the
  session's model for every role", labelled "(Recommended)", then up to 3
  mixes of the other tiers the harness accepts (for example a stronger tier
  for planner and reviewer only). A typed answer may name a model per role.
  The session model is whatever this session is running on (for
  `clud --deepseek` or another provider, the model that route resolved);
  never assume a fixed model name.
- **Local CI**, asked only when both hold:
  - `docker info` succeeds. Otherwise print
    "Docker/github actions disabled due to no docker running" and set CI off.
  - `.github/workflows/ci.yml` exists. Otherwise print
    "No .github/workflows/ci.yml; local GitHub Actions run skipped" and set CI off.

  The question: run the relevant `ci.yml` job locally under `act` before
  each push?
- **Scripts**, asked once per run and only when the `clud grind-scripts`
  report above found a `./lint` or `./test`: "Found `./lint` and `./test`.
  Run them before each push?" (name only what was found). Options: lint and
  test together, each alone when both exist, and up to four test modes (for
  example `./test --integration`) discovered by *reading*, never executing,
  the delegation files the report lists; their help text and comments give
  each option's description. Always include a "Neither" option, and keep the
  total at 4 options (the most likely ones); the user can type any other.
- **Feature merge policy** (asked once, only when the plan has feature
  children): `auto` merges the feature PR once it is green, respecting
  branch protection; `decide later` leaves the draft feature PR open for the
  user; `comment only` keeps the PR draft and posts one result comment on
  the meta issue. Never `--admin`. Record the answer as `feature_merge`
  (`auto`, `later` or `comment`) in `run.json` and the plan.
- **Problem reporting** (always): a new issue per problem (Recommended), or
  one comment per problem on the relevant issue.

## 2b. Regroup (meta of metas)

Only when 1b returned `regroup` and the user accepted it, after the
question round and before prework:

- One sub-meta issue per feature group; bug children stay direct children of
  the top meta `T`. With exactly one feature group, create no sub-meta: `T`
  itself is the feature meta.
- Classify the existing sub-meta issues under `T` by the marker
  `<!-- grind:v1 -->` (label `grind:meta`). Grind-made (marked) sub-metas are
  kept as they are, and their children are never re-parented.
- Reuse user-made (unmarked) sub-metas first, in place: rewrite each with
  `gh issue edit <n> --title ... --body-file <f>`, where the new body starts
  with `<!-- grind:v1 -->` and ends with
  `<details><summary>Previous content</summary>\n\n<old body>\n\n</details>`.
  Only groups beyond the reused count get new issues: `gh issue create` with
  the marker body and label `grind:meta`, then
  `gh api -X POST repos/<o>/<r>/issues/<T>/sub_issues -F sub_issue_id=<id>`.
- A leftover user-made sub-meta (more sub-metas than groups) is rewritten to
  say `no children after regrouping`, its old body kept in the same
  `<details>` block.
- Move each feature child with
  `gh api -X POST repos/<o>/<r>/issues/<sub>/sub_issues -F sub_issue_id=<child id> -F replace_parent=true`.
- **Nesting guard.** Before moving anything, check that every move keeps the
  depth at or below 8 levels and each parent at or below 100 sub-issues. If
  any move would break either limit, stop cleanly before prework: create or
  move nothing further, report `regroup failed: <reason>`, and fall back to
  `simple`.
- **Undo record.** Append every change to `run.json` under `"undo": [...]`,
  one entry per change, recorded before its command runs:
  `{"op": "create", "issue": n}`,
  `{"op": "reparent", "issue": child, "from": old_parent, "to": new_parent}`,
  `{"op": "rewrite", "issue": n, "title": old_title, "body": old_body}`.

Then set the plan's `structure: "meta_of_metas"`, the chosen feature stage's
`sub_meta` to its sub-meta number (`null` when `T` is the feature meta), and
`deferred_groups` for the other groups. `meta` in `run.json` stays the top
meta `T`.

## 3. Record the run

Write `.clud/grind/run.json` at the repository root:

```json
{"mode": "parallel", "ci": false, "scripts": {"lint": "./lint", "test": "./test --integration"},
 "preflight": {"action": "stash", "branch": "main", "stash": "grind-<run-id>"},
 "feature_merge": "later", "problem_reporting": "issue",
 "tracks": {"12": "bug", "13": "feature"}, "meta": 100,
 "undo": [], "waiting_on_pr": null}
```

`undo` is the section 2b change record (empty without a regroup).
`waiting_on_pr` is the open feature PR that made the run bugs-only under the
1b no-overlap rule, or `null`.

`scripts` holds the chosen commands; omit a key the user did not pick, and
set `scripts` to `null` (or omit it) when they chose "Neither" or none were
found. `preflight.action` is `stash`, `wip` (with `"wip":
"wip/grind-<run-id>"` instead of `stash`), `carry` (with `"stash":
"grind-<run-id>-carry"`) or `none` (clean tree); `branch` is the
starting branch. `preflight` is what Finish restores, and nothing else. `feature_merge` is `auto`, `later` or `comment`, omitted or
`null` when the run is bugs-only. Section 4b adds
`"feature": {"branch", "worktree", "pr"}` when the feature stage starts. `problem_reporting` is `issue` or `comment`.
`tracks` maps each child to its 1b track. `meta` is the meta issue number
(the top meta issue for a meta of metas).

clud's command hook reads it to apply the per-role caps (the planner may add
worktrees only in parallel mode; the integrator may run `act` only when CI is
on; `grind-prework` may comment only on `meta`). Delete it when the run ends.

## 3b. Assemble the plan

The contract (public vs local fields, marker, size split, status comment)
lives in `docs/architecture/grind.md`, "Prework and the plan comment". Build
the `grind-plan/v1` object from intake, the 1b plan, the preflight and the
answers, keeping the real preflight details locally:

```json
{"schema": "grind-plan/v1", "run_id": "1f3a", "meta": 100, "original": 95,
 "repo": "zackees/clud", "main": "main", "mode": "parallel",
 "preflight": {"action": "stash", "branch": "main", "stash": "grind-1f3a"},
 "structure": "simple",
 "stages": [
   {"stage": "bugs", "base": "main", "children": [101, 103]},
   {"stage": "feature", "group": "auth rework", "sub_meta": null,
    "branch": "grind/meta-100-1f3a", "base": "grind/meta-100-1f3a",
    "children": [102, 104, 105], "depends_on_bugs": {"104": [103]}}],
 "deferred_groups": [], "feature_merge": "auto", "problem_reporting": "issue",
 "models": {"planner": "…", "worker": "…", "reviewer": "…", "integrator": "…"},
 "ci": false, "scripts": {"lint": "./lint", "test": "./test"},
 "rules": {"stuck_bug": "block_dependents_only", "no_overlap": "bugs_only"}}
```

Only one feature stage appears in `stages` per run; other feature groups
wait in `deferred_groups`. Write it to `.clud/grind/plan.json` at the repository root. The workflow's
first agent, `grind-prework`, posts the public copy (preflight reduced to
`"handled"`) under `<!-- grind:v1 plan run=<run-id> -->`, and every later
agent gets that comment's URL. Nobody edits the plan comment.

## 4. Run

- Cron: read `../grind-cron/SKILL.md` and follow it with the goal source and the answers.
- Parallel or sequential: start the Workflow named `grind-run` with args
  `{repo, main, mode, goals, ci, scripts, plan, meta, models: {planner,
  worker, reviewer, integrator}}`, where `plan` is the 3b object. Omit a
  model the user left at the session default, and omit `scripts` when none
  were chosen. As in 1b, its result arrives as a task notification; Finish
  starts only after it arrives, and nothing is asked while waiting.
- If the workflow returns `stopped: 'prework'`, report that the plan could
  not be posted and that no work was done, then go to Finish.

**Status comment (router only).** When the workflow starts (or just
before), post one `<!-- grind:v1 status run=<run-id> -->` comment on the
meta issue with one line per child. Edit it with
`gh api -X PATCH repos/<o>/<r>/issues/comments/<id> -f body=...` as results
arrive and once more at Finish.

Concurrency is fixed by the workflow: at most 4 planner/worker/reviewer
agents at once, and exactly one integrator.

## 4b. Feature stage (feature-branch mode)

Only when the plan has feature children, at the start of the feature stage
(after the bug stage ends):

1. `git fetch origin <main>`.
2. Create exactly ONE worktree:
   `git worktree add <repo>/.clud/grind/worktrees/feature -b grind/meta-<M>-<run-id> origin/<main>`,
   where `<M>` is the meta issue.
3. With `preflight.action: carry`, apply the carried stash in that
   worktree (`git -C <worktree> stash pop <stash@{n} of grind-<run-id>-carry>`)
   and commit it as the branch's first commit,
   `grind: carry uncommitted changes from <starting branch>`. Then push the
   branch: `git -C <worktree> push -u origin grind/meta-<M>-<run-id>`.
4. Before the first goal lands, open a DRAFT feature PR:
   `gh pr create --draft --base <main> --head grind/meta-<M>-<run-id>`. The
   body starts with `Closes #<meta>` (plus `Closes #<original>` when intake
   converted an original issue), followed by a goals table (goal, goal PR,
   status). For a meta of metas, `<meta>` is the feature group's sub-meta;
   the top meta is never in a `Closes` line. The lander adds each goal's
   `Closes #N` line, updates the table and labels the issue
   `grind:on-feature` as the goal lands.
5. Record `{"feature": {"branch": "…", "worktree": "…", "pr": "…"},
   "feature_merge": "…"}` in `run.json`, and pass `feature` and
   `feature_merge` to `grind-run`.

The router creates no other worktree and never touches the user's checkout
after preflight. If `origin/<main>` moves during the stage, merge it into the
feature branch (`git merge origin/<main>`); never rebase the feature branch.

## 5. Finish (always, as the very last step)

After the workflow returns, whether or not every goal merged:

1. **Report** each goal's PR and whether it merged, and list anything left
   open with its reason. For a feature stage, by `feature_merge`:
   - `later`: report the open draft feature PR.
   - `comment`: post one result comment on the meta issue (branch, goals
     landed, feature PR) and keep the PR draft.
   - `auto`: report the feature PR as merged, or as "waiting for review".
2. **File problems (router only).** Gather every `problems` item from the
   workflow result (each goal entry's `problems`, `feature.problems`, and the
   top-level `problems`), and dedupe by kind + summary + related_issue. Then,
   by the plan's `problem_reporting`:
   - `issue`: create the label first if missing
     (`gh label create grind:followup --force`). For each problem, search for
     an existing follow-up with the same title
     (`gh issue list --state all --label grind:followup --search '<summary> in:title'`)
     and skip duplicates; otherwise run
     `gh issue create --title '<kind>: <summary>' --label grind:followup --body-file <f>`,
     where the body holds the evidence, `Refs #<meta>`, and the marker
     `<!-- grind:followup meta=<meta> stage=<bugs|feature> feature-pr=<n or none> -->`
     (`stage=bugs` when the problem's goal was a bug-stage goal or the run
     has no feature stage). NEVER attach a follow-up as a sub-issue of the
     meta issue, and never add it to the meta's Tracks list.
   - `comment`: one `gh issue comment` per problem on its `related_issue`,
     or on the meta issue when there is none, after the same duplicate
     search.

   If a create or comment fails, keep going, and list that problem inline
   (kind, summary, evidence) in the final report under "Problems not filed".
   Follow-ups never block the meta issue closing or the no-overlap rule,
   because they are not sub-issues.
3. **No files left behind.** Remove `.clud/grind/run.json` and
   `.clud/grind/plan.json`. Remove every
   worktree and temporary branch the run created, but only after checking
   it has no unpushed work (see `/clud-git`); push every worktree before
   removing it. Never delete a `grind/*` branch while its feature PR is
   open; Finish leaves those branches for the feature PR's merge. A merged
   goal PR's commits stay reachable at `refs/pull/<n>/head`, but an
   unpushed worktree has no such copy, so push first. Then `git status --porcelain`
   prints nothing: no untracked files, no uncommitted changes, and no stash
   the run created other than the preflight stash step 4 pops.
4. **Restore only what `preflight` recorded.** `git fetch origin`, then
   `git switch <preflight.branch>`; run `git pull --ff-only origin <main>`
   only when that branch is `<main>`. Then, by `preflight.action`:
   - `stash`: `git stash pop stash@{n}`, where `stash@{n}` is the entry
     named `grind-<run-id>` in `git stash list` (never a bare pop). If it
     conflicts, leave the stash in place and report it.
   - `wip`: leave `wip/grind-<run-id>` local, never pushed, and stay on
     the starting branch; report the branch name.
   - `carry`: nothing to restore; the changes are the feature branch's
     first commit. Report that.
   - `none`: nothing beyond the switch.
5. **Report what you could not clean or restore**, and why. Never delete
   work the run did not create to get a clean status, and never ask about
   repo state here; report it instead.

## The DAG

| Node | Role | Caps |
|---|---|---|
| `/grind-intake` | router | main session |
| `/grind-prework` | `grind-prework` | read-only git/gh; `gh issue comment` on the meta issue only; no edits, worktrees or builds |
| `/grind-plan` | `grind-planner` | read-only git/gh; `git worktree add` in parallel mode |
| `/grind-plan` (plan-only) | `grind-planner` | read-only git/gh; no Write/Edit, worktree or push |
| `/grind-work` | `grind-worker` | read/write assigned files; read-only `gh`; web search |
| `/grind-review` | `grind-reviewer` | same as worker |
| `/grind-integrate` | `grind-integrator` | full shell, one at a time; no `bosn`, containers only via `act`, no new worktrees |
| `/grind-land` | `grind-lander` | `gh pr`, `pr_merge_watch`, `git push`; no edits |
| `/grind-cron` | router | main session |
| Finish: file problems | router | main session; the only node that runs `gh issue create` (`grind:followup` issues) |

Tools are capped by each agent type's `tools:` list; shell commands by
clud's PreToolUse hook, keyed on the agent type.
