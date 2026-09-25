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
