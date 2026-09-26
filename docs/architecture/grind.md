# `grind` Execution Contract

This document is the single owner of the required behavior of `clud grind`
and the `/grind` skill DAG it launches. Current behavior that conflicts with
it is a bug, not a compatibility contract.

## Launch

`clud grind [url]` starts exactly one normal, foreground, interactive PTY
session for the Claude harness. With no URL, clud resolves the repository's
`origin` remote to its forge issues page; an explicit URL is used verbatim.
The seeded prompt is:

```text
/grind <resolved URL>
```

clud passes it to the harness's normal interactive entrypoint. It does not
use a headless prompt flag or subcommand (`-p`, `exec`), relaunch the backend,
issue an external repeat prompt, inject or poll DONE/BLOCKED markers, set an
iteration count, use the daemon repeat worker, or enable stream-json
rendering. Everything after the prompt belongs to the harness.

`grind` requires the Claude harness: `/grind` drives Claude Code's Workflow
tool and, in cron mode, its native `/loop`. A Codex or DeepSeek model uses
`--harness claude`. Other harnesses are refused before launch
(`command::builder::grind_launch_error`); clud never substitutes `clud loop`
or a hand-rolled loop.

## The DAG

`/grind` is a router over bundled skills, one bundled workflow, and five
capped agent types:

```
/grind ─ reconcile ─ /grind-intake (routing) ─ grind-run planOnly (classify) ─ preflight ─ ONE question round ─┐
  ┌─────────────────────────────────────────────────────────────────────────────────────────────────────────┘
  ├─ parallel   ─┐
  ├─ sequential ─┴─ workflow grind-run: prework → [bug stage, then feature stage]: plan → work → review → integrate → land ─ Finish
  └─ cron ─ /grind-cron ─ /loop: one sequential run per tick
```

The sections below follow that order: [routing](#input-routing), the
[planning pass](#plan-only-classification-pass) and its threshold,
[meta of metas](#meta-of-metas-and-no-overlap),
[preflight and the question round](#preflight-and-the-single-question-round),
[problem reporting](#problem-reporting),
[prework](#prework-and-the-plan-comment), the
[two stages](#bug-stage-then-feature-stage) and
[feature-branch mode](#feature-branch-mode).

| Asset | Source | Installed to |
|---|---|---|
| Skills `grind`, `grind-intake`, `grind-prework`, `grind-plan`, `grind-work`, `grind-review`, `grind-integrate`, `grind-land`, `grind-cron` | `crates/clud-bin/assets/skills/` (`skills.rs::BUNDLED_SKILLS`) | `~/.claude/skills/`, `~/.codex/skills/` |
| Agents `grind-prework`, `grind-planner`, `grind-worker`, `grind-reviewer`, `grind-integrator`, `grind-lander` | `crates/clud-bin/assets/agents/` (`claude_files.rs`) | `~/.claude/agents/` |
| Workflow `grind-run` | `crates/clud-bin/assets/workflows/grind-run.js` (`claude_files.rs`) | `~/.claude/workflows/` |

Each workflow agent runs as its `grind-<role>` type and is told to invoke
its leaf skill, so the procedure lives once, in the skill, and each leaf is
also usable directly (for example `/grind-land` on an existing PR).

### Only `/grind` is model-facing

The leaves and roles exist for the workflow, and for a person testing one by
hand; a model must never pick them up from an ordinary prompt.

- **The leaf skills** (`grind-intake` … `grind-cron`) are
  `disable-model-invocation: true`. The model never sees them, but a user can
  still type `/grind-work` to try one. `/grind` itself stays invocable, because
  `clud do` tells the model to use it. The router reads `grind-intake` and
  `grind-cron` as files next to its own directory instead of invoking them.
- **The agent types** ignore that frontmatter, so they're still listed to the
  model. clud's PreToolUse hook refuses any `Agent` call whose
  `subagent_type` is `grind-*`. The workflow's `agent()` spawns don't go
  through the `Agent` tool, so the workflow is unaffected.
  `CLUD_ALLOW_GRIND_AGENTS=1` lifts the block for testing a role by hand.
- **Each agent carries its procedure.** A hidden skill can't be loaded with
  the `Skill` tool or `skills:` preload, so `claude_files.rs` writes each
  agent file with its leaf skill's body appended. The skill remains the
  single source.

### Router questions

A workflow cannot ask questions while it runs, so the `/grind` skill asks
everything first:

- **Mode.** *Parallel*: each goal gets a git worktree; workers only read and
  write. *Sequential*: one goal at a time from `origin/<main>` in the local
  checkout, no worktrees or sister clones, so a heavy C++/Rust build cache is
  reused. *Cron*: the harness's `/loop`, one issue per tick, each tick a
  sequential run.
- **Models** for planner, worker, reviewer and integrator (the lander shares
  the integrator's). The default is the session's own model, whatever route
  resolved it; an unchanged default is omitted so the agent inherits it.
- **Local CI**: offered only when `docker info` succeeds and
  `.github/workflows/ci.yml` exists. Otherwise the router prints why
  ("Docker/github actions disabled due to no docker running", or that
  `ci.yml` is missing) and CI is off.

These are part of the single up-front round described in
[Preflight and the single question round](#preflight-and-the-single-question-round).
The router records the answers (`mode`, `ci`, `scripts`, `preflight`,
`feature_merge`, `problem_reporting`, `tracks`, `meta`, and later `feature`)
in `.clud/grind/run.json` at the repository root for the hook below, and
removes it when the run ends.

### Input routing

Issue [#1404](https://github.com/zackees/clud/issues/1404); the steps live
in [`grind-intake/SKILL.md`](../../crates/clud-bin/assets/skills/grind-intake/SKILL.md).
Every run works on a meta issue:

- **One issue.** The bundled `github/is_meta_issue.py` tool alone decides
  meta-ness. A meta issue's open children are the goals. A multi-part
  non-meta issue gets intake's one question, *Convert #N into a meta
  issue?*: convert splits it into children under a new meta (the original
  stays open); declining, or a single-change issue, refuses with
  ``run `/do N` `` and creates nothing.
- **Issue list.** A new meta issue tracks the given issues as sub-issues.
- **Prompt.** Split into deliverables, one child issue each, under a new meta.
- **Nothing.** Pick from the open issues, then route as a list.
- **Stops that create nothing.** A meta issue whose children are all closed
  (`Nothing to do`), a `gh` failure reported by the tool (never read as "not
  meta"), and a conversion that fails partway (the partial state is listed)
  all end the run before any question, `run.json` or worktree.

**Tracks.** After the plan-only pass each child carries a track, `bug`
(lands on `<main>` in the bug stage) or `feature` (lands on the feature
branch), recorded in `run.json` `tracks`; see
[Bug stage, then feature stage](#bug-stage-then-feature-stage).

### Plan-only classification pass

Issue [#1406](https://github.com/zackees/clud/issues/1406). Before asking
anything about a meta issue, the router writes `{"phase": "plan"}` to
`run.json` and starts `grind-run` with `planOnly: true, meta`. One
`grind-planner` classifies every child as a bug (`#N bug → main`) or a
feature (`#N feature → grind/meta-<T>-<group>`), names feature groups and
their independence, and returns without starting any other role. The hook
refuses the planner `git worktree add` and file writes in this phase.
The complexity threshold is
`grind-run.js::thresholdVerdict`: it picks `path: regroup` only for a confident
classification with every child placed, 8+ children, and at least 2
independent feature groups of 3+; otherwise it logs
`keeping #T as is: <reason>` and the meta issue is left untouched. The
router then rewrites `run.json` without `phase` for the real run.

### Meta of metas and no overlap

Issue [#1412](https://github.com/zackees/clud/issues/1412). The commands
live in the router skill, section 2b
([`grind/SKILL.md`](../../crates/clud-bin/assets/skills/grind/SKILL.md));
this is the contract.

- **Regroup only after confirmation.** `path: regroup` is a proposal until
  the user confirms it in the question round.
- **Shape.** One sub-meta per feature group; bugs stay directly under the
  top meta. A single feature group gets no sub-meta.
- **User-made sub-metas** are reused in place: the old body is kept in
  `<details>Previous content</details>`, surplus groups get new sub-metas,
  and leftovers are rewritten as `no children after regrouping`.
- **Grind-made sub-metas** (marker `<!-- grind:v1 -->`, label `grind:meta`)
  are kept and their children are not touched.
- **Moves** use `replace_parent`. The 8-level depth / 100-children guard
  fails the run before prework, not midway.
- **Undo.** Every change is recorded in `run.json` `undo`.
- **One feature group per run.** The rest go to `deferred_groups`.
- **No overlap.** An open feature PR under the same top meta makes the run
  bugs-only (`waiting_on_pr`, rule `rules.no_overlap`). A feature PR under a
  different top meta does not block.
- `grind-run.js` enforces the deferral and the no-overlap rule, not just the
  router.
- **Closing the top meta.** The chosen group's feature PR carries
  `Closes #<sub-meta>` and its children, never the top meta: feature PRs
  merge in any order, so no single PR may close it. Bug children close
  through their own PRs. No grind role closes the top meta.

### Preflight and the single question round

Issue [#1407](https://github.com/zackees/clud/issues/1407); spec
[#1392](https://github.com/zackees/clud/issues/1392) §0 and §2.

- **Order.** Routing → planning pass → preflight → **one** question round
  (at most 2 `AskUserQuestion` calls) → prework → run. Nothing is asked
  after prework starts.
- **Dirty repo.** Preflight inspects the checkout. The options are: stash
  as `grind-<run-id>`; commit to a local-only branch `wip/grind-<run-id>`;
  carry the changes (offered only when a feature stage exists); or abort,
  which creates nothing.
- **Recording.** The answers go into `run.json` as `preflight` (what was
  stashed or branched), `feature_merge`, `problem_reporting` and `tracks`.
  Finish restores only what `preflight` recorded.
- **Enforcement.** `block_bad_cmd_grind_caps::tool_reason` denies
  `AskUserQuestion` to every `grind-*` subagent. The main-session router is
  bound by its skill text instead, pinned by
  `tests/harness/test_grind_upfront.py`.

The rationale is in
[DD-097](../DESIGN_DECISIONS.md#dd-097-the-grind-run-plans-before-it-asks-one-up-front-question-round-none-after-prework).

### Problem reporting

Issue [#1411](https://github.com/zackees/clud/issues/1411). The
`problem_reporting` answer is recorded as described in
[Preflight and the single question round](#preflight-and-the-single-question-round).

- **Returned, not filed.** Every role may return
  `problems: [{kind, summary, evidence, related_issue}]` alongside its
  result. Roles never file anything themselves.
- **Collected once.** `grind-run` collects the problems from every role and
  dedupes them by `kind` + `summary` + `related_issue`.
- **Only the router files.** It follows `problem_reporting`:
  - `issue`: one issue per problem, body carrying `Refs #<meta>`, label
    `grind:followup`, and the marker
    `<!-- grind:followup meta=<T> stage=<stage> feature-pr=<N> -->`. It is
    never attached as a sub-issue of the meta.
  - `comment`: one comment per problem on its `related_issue`, or on the
    meta issue when there is none.
- **Never blocking.** Follow-ups do not hold up closing the meta issue and
  are not counted by the no-overlap check.
- **Later pickup.** Intake and cron treat a `grind:followup` issue from the
  bug stage as eligible at once; one from a feature stage becomes eligible
  only after that stage's feature PR has merged. The label is removed when
  the issue is picked up.
- **Filing failures.** A failed filing does not stop the run; the router
  lists each unfiled problem inline in its final report.

The rationale is in
[DD-098](../DESIGN_DECISIONS.md#dd-098-grind-follow-up-issues-are-never-sub-issues-of-the-meta).

### Prework and the plan comment

Issue [#1408](https://github.com/zackees/clud/issues/1408); spec
[#1392](https://github.com/zackees/clud/issues/1392) §0.

- **Role.** `grind-prework` is the first agent of the real run, after the
  question round and before any planner or worker. It does not plan; it
  records the plan the router assembled.
- **Local copy.** The router writes the full `grind-plan/v1` object to
  `.clud/grind/plan.json` and passes it to `grind-run` as `args.plan`, with
  the meta issue as `args.meta` (the top meta issue in a meta of metas).
  `run.json` also carries `meta`, so the hook knows which issue prework may
  comment on. Finish deletes `plan.json`.
- **Public comment.** Prework posts the plan once on the meta issue, in a
  fenced `json` block under `<!-- grind:v1 plan run=<run-id> -->`. The
  comment is public on public repos, so local-only fields are dropped:
  `preflight` becomes `"handled"`, and the starting branch, stash name, WIP
  branch and local paths never appear. The local details stay in
  `plan.json`.
- **Never edited.** Nobody edits the plan comment, so no decision can change
  mid-run. Every later agent is told its URL and reads it before starting.
- **Size.** GitHub caps a comment at 65,536 characters. Per-child entries stay
  one line; above about 60,000 characters the plan is split into numbered
  comments marked `part=k/N`, linked from the first.
- **Failure stops the run.** If prework cannot post, the workflow returns
  `stopped: 'prework'` before any other agent starts; the router reports
  that the plan was not posted and no work was done.
- **Status comment.** Results live in a separate comment,
  `<!-- grind:v1 status run=<run-id> -->`, one line per child. The router,
  not prework, posts it when the run starts and edits it
  (`gh api -X PATCH repos/<o>/<r>/issues/comments/<id>`) as results arrive
  and at Finish.
- **Caps.** Read-only git and `gh`, plus `gh issue comment` on the
  `run.json` meta issue only. No Write/Edit, builds or worktrees.

### Bug stage, then feature stage

Issue [#1409](https://github.com/zackees/clud/issues/1409). The stage fields
are defined by the plan; see
[Prework and the plan comment](#prework-and-the-plan-comment).

- **Order.** `grind-run` runs every bug-stage goal against `origin/<main>`
  first. Only then does it run each feature stage, against that stage's
  `stage.base`. A goal named in no stage runs with the bug stage. With no
  bug stage, or no feature stage left after the no-overlap and deferral
  filters, the goals run as one batch.
- **One workflow call.** Both stages run inside one `grind-run` call, and
  the main session cannot act between them. So the router sets up the
  feature branch and its draft PR before it starts the workflow (see
  [Feature-branch mode](#feature-branch-mode)).
- **Base override.** For a run without a plan, `args.base` replaces the
  default base (`origin/<main>`).
- **Stuck bug.** The rule is `block_dependents_only`. When a bug does not
  merge, only the feature children that list it in `depends_on_bugs` are
  blocked, reported as `blocked: bug #N did not land`. Every other goal
  proceeds.

### Feature-branch mode

Issue [#1410](https://github.com/zackees/clud/issues/1410); spec
[#1392](https://github.com/zackees/clud/issues/1392) §6. Used for
feature-stage children of a meta issue; issue safety is in
[#1393](https://github.com/zackees/clud/issues/1393).

- **Setup.** Before it starts `grind-run`, the router creates
  `grind/meta-<M>-<run-id>` from `origin/<main>` in the worktree
  `.clud/grind/worktrees/feature` and pushes it. `<M>` is the feature meta:
  the meta issue, or the chosen group's sub-meta in a meta of metas. It
  opens the **draft feature PR** into `<main>` (body `Closes #<M>`, plus
  `Closes #<original>` for a converted issue) and passes
  `feature: {branch, worktree, pr}` and `feature_merge` to the workflow and
  `run.json`. The user's checkout is not touched after preflight.
- **Bug fixes reach the branch by merge.** The branch is cut before the bug
  stage runs, so the first feature-stage integrator merges the updated
  `origin/<main>` into it (see *Merge commit, main merged in* below). That
  is how the feature stage builds on the bugs that landed.
- **Goals.** Each goal PR's base is the feature branch, so it still gets CI
  and a review trail. The integrator rebases the goal onto the feature
  branch, writes `Refs #N`, and appends `Closes #N` to the feature PR's body,
  keeping `Closes #<M>`. The lander merges the goal PR into the feature
  branch with `--merge`. Parallel goal worktrees branch from the feature
  branch; sequential goals run inside the feature worktree.
- **Merge policy** (`feature_merge`, asked up front):
  - `auto`: once CI is green the lander marks the PR ready and runs
    `gh pr merge --merge`, never `--admin`, so branch protection and required
    reviews still apply.
  - `later`: the router reports the PR and leaves it open, still a draft,
    for the user; the hook refuses the lander `gh pr ready` and the merge.
  - `comment`: the router posts the result on the meta issue and keeps the
    PR a draft.
- **Merge commit, main merged in.** The feature PR lands as a merge commit.
  If `<main>` moves during the run, `<main>` is merged into the feature
  branch, never rebased, so goal SHAs already merged there stay unchanged.
- **Caps.** `block_bad_cmd_grind_caps.rs` refuses `gh pr merge --admin` on
  the feature PR, lets the lander merge the feature PR into `<main>` only
  under `feature_merge: auto`, and refuses deleting a `grind/*` branch while
  its feature PR is open. The router's single worktree (the feature one) is
  enforced by the `/grind` skill text, not the hook, since the router is the
  main session.

The rationale is in
[DD-095](../DESIGN_DECISIONS.md#dd-095-the-grind-feature-lands-as-a-merge-commit-and-main-is-merged-into-the-feature-branch-rather-than-rebased).

### Never losing issues in feature mode

Issue [#1393](https://github.com/zackees/clud/issues/1393).

- **Single closer.** Goal PRs into the feature branch say `Refs #N`, never a
  closing keyword. Only the feature PR carries `Closes #N` for each landed
  child plus `Closes #<meta>`, so GitHub closes an issue exactly when its fix
  merges into `<main>`. No role runs `gh issue close` on a feature child.
- **Label and marker.** When a goal merges into the feature branch the lander
  adds the `grind:on-feature` label and posts one marker comment:
  `<!-- grind:v1 feature-pr=#<N> branch=<feature> goal-pr=#<G> run=<run-id> -->`.
  The label makes pending issues queryable; the last marker names the feature
  PR. Intake and triage skip labelled issues.
- **Reconcile.** `clud grind reconcile` (`grind_reconcile.rs`) runs at the
  start of every `/grind` run and every `/grind-cron` tick. For each labelled
  issue it reads the marker, the feature PR, and (for a closed issue) the
  GraphQL `ClosedEvent.closer`:

  | Feature PR | Issue | Action |
  |---|---|---|
  | merged into `<main>` | open | close citing the feature PR; remove label |
  | merged into `<main>` | closed | remove label |
  | closed unmerged | any | reopen if closed; remove label; comment citing `refs/pull/<N>/head` |
  | open, or merged elsewhere | closed by hand, by an unmerged PR, by a PR into a non-default base, or by a commit not on `<main>` | reopen with a comment naming the closer |
  | open, or merged elsewhere | closed by a PR merged into `<main>` or a commit on `<main>` | none |
  | open and stale (14 days idle, conflicting, or branch gone) | any | report stale; comment on the meta issue |

- **Reachable commits.** A commit closer counts as landed only when
  `compare/<main>...<oid>` reports `behind` or `identical`, i.e. the commit is
  reachable from `<main>`.
- **Branches.** `grind/*` branches are not deleted while their feature PR is
  open (see the caps above and the `/clud-git` playbook), so an unmerged
  feature's commits stay reachable.

Reconcile is idempotent: a healthy open issue yields no action. It is covered
by unit tests in `grind_reconcile.rs` and by
`tests/harness/test_grind_reconcile.py`. The rationale is in
[DD-096](../DESIGN_DECISIONS.md#dd-096-the-feature-pr-is-the-single-closer-of-grind-issues-and-clud-grind-reconcile-reopens-early-closes).

### Repository lint/test scripts

Issue [#1336](https://github.com/zackees/clud/issues/1336).

- **Detection.** The router carries a `` !`clud grind-scripts` `` line
  (`command/grind_scripts.rs`), so Claude Code renders the detected facts into
  the skill before the model sees it. The candidates are `lint`, `lint.sh`,
  `lint.bat`, `lint.ps1` and the matching `test*`. On Windows the `.bat` /
  `.ps1` form wins, with the extensionless or `.sh` script (via Git Bash) as a
  fallback. Elsewhere only the extensionless or `.sh` form counts, so a lone
  `test.bat` on Linux means nothing was detected. If a script isn't
  executable, it runs through its interpreter: `bash ./lint`,
  `cmd /c lint.bat`, `pwsh -File lint.ps1`.
- **Modes, by reading only.** For each script the output lists the files to
  read: the script and whatever it delegates to (`python -m ci.test` means
  `ci/test.py`; `bash ci/x.sh` means `ci/x.sh`). The router reads them and
  offers at most four user-facing modes, such as `--integration`, each with a
  short description. It ignores flags the script passes to its own tools
  (`uv`, `cargo`, `pip`, `pytest`). It never runs a script to find its modes,
  because a script that ignores `--help` would start the whole suite.
- **One question per run**, asked only when something was detected: lint and
  test, lint and test with each mode found, lint only, test only, or neither
  (use the planner's verify commands). If nothing was detected, there's no
  question.
- **Recording.** The answer goes into `.clud/grind/run.json` as `scripts`,
  for example `{"lint": "bash ./lint", "test": "bash ./test --integration"}`,
  and reaches the workflow as `args.scripts`. `{}` means neither.
- **Integrator.** Before **every** push, fix rounds included, it runs the
  goal's focused RED → GREEN test, then lint, then test. A failure loops
  inside the integrator (fix, re-run), because nothing has been pushed yet,
  so it doesn't use up a lander fix round. When scripts are chosen, the
  planner supplies only the focused test.
- **Long scripts.** Anything that may run past the Bash tool's 600-second
  limit runs in the background, and the integrator waits for its exit code.
  Never pipe a script through `tail`, which hides its exit status (#1331).
- **Main already red.** If lint or test already fails on `origin/main`, the
  integrator fixes that too. The fix goes in its own commit ahead of the
  goal's commit, in the same PR, titled like
  `fix: pre-existing lint failure on main`.
- **Caps.** The integrator's shell is a denylist, so the scripts run.
  `grind-worker` and `grind-reviewer` are denied them like any other build,
  lint or test command.
- **Checks are never worker tasks (#1397).** Every planned task writes at
  least one file. `grind-run.js::routeCheckTasks` moves a file-less task
  (for example "verify build and tests") into the integrator's verify
  commands, and a plan left with no file-writing tasks ends the goal
  unmerged instead of running an empty review.

The rationale is in
[DD-091](../DESIGN_DECISIONS.md#dd-091-grind-gets-repo-linttest-scripts-from-a-clud-subcommand-asked-once-per-run).

### Integration order

- Plan, work and review run at most **4** agents at a time.
- Exactly **one** integrator runs at a time. It is the only role that
  builds, so builds never overlap and caches stay warm. No `bosn` wrapper is
  needed or allowed.
- The planner declares `depends_on`. An isolated goal rebases onto
  `origin/<base>` (`<main>` in the bug stage, the feature branch in the
  feature stage). A dependent goal waits until its dependency merges, then
  rebases onto the new `origin/<base>`.
- Landing does not hold the build lock: while one PR's CI runs on GitHub, the
  next goal integrates locally.
- The lander runs `pr_merge_watch`. When checks pass it admin-merges a
  bug-stage PR into `<main>`, or merges a feature-stage goal PR into the
  feature branch with `--merge`. On red CI or
  new review it hands the failure back to the integrator, which fixes,
  re-verifies and pushes. That is at most 10 rounds; after that the PR is
  left open and reported.
- `pr_merge_watch` exits `0` green, `1` failed, `2` new review, `3` closed,
  `4` timeout, `5` approval required (fork `action_required`; report, no
  retry), `6` never reported (required check never ran; blocked), `7` stale
  (older than 14 days; re-run once), `8` no checks (no workflows, a
  `[skip ci]` marker, or nothing registered within the grace period; merge
  when `mergeStateStatus` is `CLEAN`), `9` conflict (back to the integrator),
  `10` GitHub unreachable (never reported as closed), `11` queued too long
  (opt-in `--max-queued`). The lander passes `--timeout 540`, below its
  600 s tool cap, so the watch always exits on its own; a watch never cancels
  runs on a head SHA it did not start on (#1418). It judges each check by the newest run of
  its workflow, so a superseded cancelled run is not a failure; the full rule
  lives in the watcher's docstring
  (`crates/clud-bin/assets/tools/github/pr_merge_watch.py`).

### Role caps

| Role | Tools (`tools:` frontmatter) | Shell (hook) |
|---|---|---|
| `grind-planner` | Read, Grep, Glob, Bash, WebSearch, WebFetch, Skill | read-only git and `gh`; `git worktree add` in parallel mode only, never in the plan-only phase (the hook also refuses it Write/Edit) |
| `grind-prework` | Bash, Read, Grep, Glob | read-only git and `gh`; `gh issue comment` on the `run.json` meta issue only; no edits, worktrees or builds |
| `grind-worker` | Read, Edit, Write, Grep, Glob, Bash, WebSearch, WebFetch, Skill | read-only `gh` only |
| `grind-reviewer` | same as worker | same as worker |
| `grind-integrator` | Read, Edit, Write, Grep, Glob, Bash, WebSearch, WebFetch, Skill | anything except `bosn`, direct `docker`/`podman`, `git worktree add`, and `act` when CI is off |
| `grind-lander` | Read, Grep, Glob, Bash, Skill | `gh pr view\|checks\|diff\|list\|merge`, `gh pr ready` on the feature PR under `feature_merge: auto` only, `gh run view\|list`, read-only git, plain `git push`, `pr_merge_watch` |

Claude Code enforces the tool lists. Shell commands are enforced by clud's
native PreToolUse hook (`clud-block-bad-cmd`): the harness puts the calling
subagent's `agent_type` in the payload, and
`block_bad_cmd_grind_caps.rs` applies that role's policy. Commands it cannot
decompose (command substitution, subshells) are refused for capped roles, as
are `find -exec`/`-delete`, `tail -f`, and git global options other than
`-C`. The integrator's bans also look inside wrappers such as `bash -c`.
A goal may depend only on goals listed before it, so dependency waits cannot
cycle. `CLUD_ALLOW_ALL_CMDS=1` turns the whole command hook off, these caps
included.
Other agents and the primary session are unaffected. The tests next to it
own the exact allowlists. On top of the table, every `grind-*` role is
denied `AskUserQuestion`, `gh issue create` and closing an issue.

## Tests

The workflow, the agent types and the caps run end to end on the real Claude
Code in `tests/harness/test_grind.py`
([testing-tiers.md](testing-tiers.md)). The mock backend plays each role, and
the suite checks the ordering, concurrency, fix-round and cap behavior above.
The router's questions are model behavior, which a scripted model can't
exercise, so the suite starts `grind-run` directly and pins the router text
separately.

The per-feature suites in `tests/harness/`, each named for the section above
it covers:

- `test_grind_routing.py` — input routing and the `/do` refusal.
- `test_grind_plan_only.py` — the plan-only pass and threshold.
- `test_grind_meta_of_metas.py`, `test_grind_overlap.py` — regroup and no
  overlap.
- `test_grind_upfront.py` — preflight and the single question round.
- `test_grind_prework.py` — the plan comment.
- `test_grind_stages.py`, `test_grind_feature.py` — stages and
  feature-branch mode.
- `test_grind_problems.py` — problem reporting.
- `test_grind_reconcile.py` — reconcile.
- `test_grind_scripts.py` — the repository lint/test scripts.
- `test_grind_e2e.py` — the three user scenarios posted on
  [#1392](https://github.com/zackees/clud/issues/1392), end to end, each
  from routing to Finish:
  1. a meta issue of unrelated bugs: every PR goes into `<main>` with
     `Closes #N`, and one problem becomes a `grind:followup` issue that is
     not a sub-issue;
  2. a prompt with prerequisite bugs, a feature and a dirty checkout, under
     `auto`: the checkout is stashed, the bugs land first, `<main>` is
     merged into the feature branch, and the one feature merge commit closes
     the meta issue and every feature part;
  3. a regrouped epic under `later`: the user's sub-metas are rewritten in
     place, one group runs while the other is deferred, the feature PR stays
     open, the next run is bugs-only and names it, and the user's merge
     closes the group.

GitHub itself is faked (`tests/harness/fake_gh.py`), so one step stays
manual: a smoke run against a scratch GitHub repository, checking that the
closing-keyword rule closes children only when the feature PR merges into
`<main>`, that `replace_parent` moves children during a regroup, and that
a large feature PR's body stays within GitHub's closing-keyword count.

## Boundary with `clud loop`

`clud loop` remains a separate command with its own external runner,
DONE/BLOCKED contract, iteration budget, artifacts, and optional repeat
scheduler; see [the loop subsystem](loop-subsystem.md). `grind`'s cron mode
uses the harness's `/loop`, never clud's.

## Implementation review checklist

- One backend harness session per `clud grind`, launched per DD-086 (PTY
  from a console), with a prompt starting `/grind`.
- No loop markers, repeat schedule, external iteration count, or stream-json
  setting in the plan.
- Unsupported harnesses fail before a backend process is spawned.
- A new `grind-*` role needs its agent file, a `claude_files.rs` entry, and a
  policy arm plus tests in `block_bad_cmd_grind_caps.rs`.

See [DD-087](../DESIGN_DECISIONS.md#dd-087-grind-is-a-skill-dag-with-capped-agent-roles)
for the rationale; it supersedes [DD-068](../DESIGN_DECISIONS.md#dd-068-grind-delegates-looping-to-the-interactive-harness)'s
direct `/loop` prompt.
