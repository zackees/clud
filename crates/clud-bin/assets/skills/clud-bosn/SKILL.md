---
name: clud-bosn
description: Run repeated local Docker builds under bosn so the loop stops filling the disk.
triggers:
  - When iterating locally on a Dockerfile or compose stack in this repo
  - When repeated docker builds are filling the disk or a prune is being considered
  - When the user mentions bosn
---
<!-- managed-by: clud -->

# clud-bosn

[bosn](https://github.com/zackees/bosn) owns the containers, volumes, and images a
local build loop creates: every resource is labeled with who made it and what it was
built from, and whether anything still needs it is a lease in bosn's registry rather
than a guess. Storage is bounded by ceilings with eviction, so the loop stops growing
without anyone running a prune.

Use it for **the agent's own build loop**. The problem it solves is running the same
containerized build hundreds of times, where Docker keeps every image and volume it
was told to make and has no notion of "this one is finished". The reflex it replaces
is `docker system prune`, which also destroys the warm cache you wanted to keep.

## Code Change Rule

If the work this loop is iterating on is a bug fix or feature implementation, use
RED -> GREEN: write or identify the focused failing test first, run it under the
containerized loop to see it fail, implement the scoped change, and rerun it to pass.
A faster build loop is only worth having if it is still answering a real question.

## Do not use this when

- Editing a `Dockerfile` or compose file that **ships** — production images, or a file
  written for someone else to run. The subject being Docker is not the trigger; local
  iteration is.
- Debugging a registry push, or anything about images leaving this machine.
- The user asked a Docker question and is not building anything.

A skill that fires on every mention of Docker becomes noise, and noise gets ignored.

## Before committing the agent to this path

1. `command -v bosn` — if absent, say so and stop. Install is
   `uv tool install git+https://github.com/zackees/bosn` (Python 3.11+; puts `bosn`
   and `bosn-docker` on PATH). Do not install it without being asked.
2. `docker info` must succeed — bosn manages a real engine, it does not replace one.
3. **WSL is refused outright**: `bosn` exits non-zero inside WSL, because its Windows
   loopback daemon is unreachable from there. Use a native Windows shell, macOS, or
   Linux. (`bosn-docker` does not yet refuse it, so a WSL failure can surface later
   and less clearly.)
4. bosn is manifest-driven: it needs a `bosn.toml` at the repo root declaring stacks,
   volumes, and tasks. Without one there is nothing for it to converge, and writing
   one is a change to the repo — propose it, do not add it silently.

## The loop

```bash
bosn ensure          # converge and register without running anything (pre-warm)
bosn unit            # run the task named in bosn.toml
bosn status --json   # bounded daemon/registry state
bosn gc --dry-run --json   # what retention would remove, before it removes it
```

Existing unmanaged volumes can be imported rather than recreated:
`bosn adopt --legacy clud` plans and reports, `--yes` applies.

## What it does not cover

Read this before promising a drop-in replacement:

- The Docker front door is **partial** — `bosn init` and
  `bosn-docker compose {up,down,logs,ps}` over a small compose subset only.
  `bosn-docker compose up -d` is rejected outright, not ignored.
- **No `docker` / `docker-compose` shims and no podman.** Arbitrary docker commands
  are not routed through bosn; only what is listed above is.
- **BuildKit's own layer cache is outside the label contract.** bosn manages
  containers, volumes, and images and never prunes the builder, so builder growth is
  still yours to watch.
- `docker compose up` maps to `bosn-docker compose up`, which labels and tracks the
  service *containers*; volumes created that way are not tracked.

When the work needs something in that list, say so and use Docker directly rather
than bending the task to fit the tool.
