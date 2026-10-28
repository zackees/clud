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

1. `command -v bosn` and `bosn --version` — use Bosn 0.1.29 or a later
   compatible release. If it is missing or older, report that; do not install or
   upgrade it without authorization.
2. `docker info` must succeed — bosn manages a real engine, it does not replace one.
3. Use the native host environment for the daemon and verify its connectivity;
   Windows and WSL may have different socket and path access.
4. Bosn is manifest-driven: it needs a `bosn.toml` at the repo root declaring
   stacks, volumes, and tasks. Without one there is nothing to converge; propose
   the manifest as part of the change before adding it.

## The loop

Use names actually declared by the repository's manifest. `bosn run` finds the
nearest `bosn.toml`, starts a daemon if needed, ensures the stack, and streams
bounded output. Its `--deadline-ms` sets the job deadline; `--output-limit` caps
captured output. Until bosn#789 is fixed, a limit as small as 4096 bytes can also
truncate internal Docker inspection; use at least 65536. Ctrl-C cancels its active
job, so observe an existing run rather
than starting or cancelling another one.

```bash
bosn run --stack linux --manifest bosn.toml --deadline-ms 300000 --output-limit 65536
bosn run --task test --stack linux --manifest bosn.toml --deadline-ms 1800000 --output-limit 65536
bosn jobs --json
```

For noisy tasks, send the full output to a uniquely named local log and preserve
the command's exit status. Show only a bounded failure excerpt in the conversation;
read incremental job logs when monitoring a run instead of repeating its full log.

`--stack` without a task ensures the stack; `--task` runs that declared task.
`bosn run` chooses `$BOSN_STATE_DIR`, then `$XDG_STATE_HOME/bosn`, then
`~/.local/state/bosn` (Windows: `%LOCALAPPDATA%\bosn`). Low-level daemon and
GC commands require the explicit state directory. On a Unix shell, inspect them
without deleting anything:

```bash
task_bosn_state="${BOSN_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/bosn}"
bosn daemon status --state-dir "$task_bosn_state" --json
# Set task_bosn_job_id to a job ID from `bosn jobs --json`.
bosn job logs --state-dir "$task_bosn_state" --job-id "$task_bosn_job_id" --after 0 --limit 64 --json
bosn gc owned --state-dir "$task_bosn_state" --container-ttl-secs 86400 --volume-ttl-secs 604800 --image-ttl-secs 604800 --json
```

The GC command is a preview unless both `--apply` and `--yes` are supplied.
Never prune or stop unrelated resources to make the loop run.

## What it does not cover

Read this before promising a drop-in replacement:

- Bosn offers partial Docker coverage; podman is not a supported backend.
- `bosn compose plan --file ...` is a planning interface, not a general Docker
  or Compose replacement. Arbitrary Docker commands are not automatically routed
  through Bosn.
- BuildKit's own layer cache is outside the resource-label contract. Bosn does not
  prune the builder; monitor its growth separately.

When an operation is not supported, say so and use the appropriate native tool
without implying Bosn manages its resources.
