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
/grind ─ /grind-intake ─ questions ─┬─ parallel   ─┐
                                    ├─ sequential ─┼─ workflow grind-run: plan → work → review → integrate → land
                                    └─ cron ─ /grind-cron ─ /loop: one sequential run per tick
```

| Asset | Source | Installed to |
|---|---|---|
| Skills `grind`, `grind-intake`, `grind-plan`, `grind-work`, `grind-review`, `grind-integrate`, `grind-land`, `grind-cron` | `crates/clud-bin/assets/skills/` (`skills.rs::BUNDLED_SKILLS`) | `~/.claude/skills/`, `~/.codex/skills/` |
| Agents `grind-planner`, `grind-worker`, `grind-reviewer`, `grind-integrator`, `grind-lander` | `crates/clud-bin/assets/agents/` (`claude_files.rs`) | `~/.claude/agents/` |
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

The router records `{mode, ci}` in `.clud/grind/run.json` at the repository
root for the hook below, and removes it when the run ends.

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
  `origin/<main>`. A dependent goal waits until its dependency merges, then
  rebases onto the new `origin/<main>`.
- Landing does not hold the build lock: while one PR's CI runs on GitHub, the
  next goal integrates locally.
- The lander runs `pr_merge_watch`. When checks pass it admin-merges. On red CI or
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
| `grind-planner` | Read, Grep, Glob, Bash, WebSearch, WebFetch, Skill | read-only git and `gh`; `git worktree add` in parallel mode only |
| `grind-worker` | Read, Edit, Write, Grep, Glob, Bash, WebSearch, WebFetch, Skill | read-only `gh` only |
| `grind-reviewer` | same as worker | same as worker |
| `grind-integrator` | Read, Edit, Write, Grep, Glob, Bash, WebSearch, WebFetch, Skill | anything except `bosn`, direct `docker`/`podman`, `git worktree add`, and `act` when CI is off |
| `grind-lander` | Read, Grep, Glob, Bash, Skill | `gh pr`, `gh run view|list`, read-only git, `git push`, `pr_merge_watch` |

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
own the exact allowlists.

## Tests

The workflow, the agent types and the caps run end to end on the real Claude
Code in `tests/harness/test_grind.py`
([testing-tiers.md](testing-tiers.md)). The mock backend plays each role, and
the suite checks the ordering, concurrency, fix-round and cap behavior above.
The router's questions are model behavior, which a scripted model can't
exercise, so the suite starts `grind-run` directly and pins the router text
separately.

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
