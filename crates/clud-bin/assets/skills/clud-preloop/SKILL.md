---
name: clud-preloop
description: Run a GitHub Actions workflow locally with preloop before pushing a branch to find out whether CI passes.
triggers:
  - When about to push a branch mainly to see whether CI passes
  - When the user asks to run a GitHub Actions workflow locally
  - When a workflow failed remotely and the fix needs more than one attempt
  - When the user mentions preloop
---
<!-- managed-by: clud -->

# clud-preloop

[preloop](https://github.com/preloopdev/preloop) runs `.github/workflows` locally on
hardware-isolated microVMs, speaking the official `actions/runner` protocol rather
than approximating it. `${{ }}` expressions, matrix builds, reusable workflows and
concurrency groups work as they do on GitHub, and it can run against **uncommitted
changes** — including untracked and gitignored files.

The valuable trigger is the one nobody says out loud: *"I'll push and see if CI goes
green."* That is a decision to spend a full remote matrix on a question a local run
answers. Cross-platform failures — a path literal that is absolute on one OS and not
another — are the class this catches cheapest.

## Code Change Rule

If the workflow is being changed to fix a bug or add behavior, use RED -> GREEN:
run it locally first and see it fail for the reason you believe, make the scoped
change, and rerun it to pass. Running a workflow locally makes that cycle cheap
enough to actually do, which is the point.

## Do not use this when

- The task is about the workflow **file** rather than its behavior: renaming a job,
  editing a matrix entry, fixing YAML indentation.
- Reading an existing remote run's logs. That run already happened; `gh run view` is
  the tool.
- The failing thing is not the workflow. A test that fails locally will fail here too,
  and going through a VM to learn that is slower than running the test.

## Before committing the agent to this path

1. `command -v preloop` — if absent, say so and stop.
2. **Install by verified download, not by piping a script into a shell.** preloop's
   README documents `curl -fsSL … | sh`; clud's own rule-writing guide uses that exact
   shape as its canonical example of a pipeline worth blocking, so a clud skill must
   not tell an agent to run it. Each release publishes per-platform tarballs with a
   matching `.sha256` (`preloop-cli-<arch>-<os>.tar.gz` plus `.sha256`, and a
   `sha256.sum`). Download the asset for the platform, verify it against its published
   checksum, and put the binary on PATH — the same material the install script uses.
3. **First run downloads a base image and builds a golden VM: 1–5 minutes.** After
   that each job forks in ~200–300 ms. If the task cannot afford the first-run cost,
   say so rather than starting it and stranding the work.

## The loop

```bash
preloop serve                                    # engine on 127.0.0.1:9090; -d to detach
preloop run -f .github/workflows/ci.yml --event pull_request
preloop dap <run-id>                             # hold the job at entry and inspect it
```

`preloop dap` pauses a job before it runs and exposes the live `github`, `env`,
`runner`, `job`, `steps` and `secrets` scopes. Reach for it when the YAML looks right
and the runtime context is what is actually wrong — a matrix value or event payload
that is not what the workflow assumed.

## Where local and remote differ

A green local run is strong evidence, not a guarantee. Say which one you ran.

- The base image is **curated, not GitHub's** — the official image is ~90 GB, and
  preloop deliberately omits dependencies that actions usually install themselves. A
  step that relies on something preinstalled on a GitHub runner can pass there and
  fail here, or the reverse. Custom and OCI-based goldens are supported.
- On Apple Silicon, x86_64 goldens run under Rosetta 2 translation, and **Docker
  actions are not supported on that path** — amd64 images inside the VM's Docker fail
  with a `rosetta-wrapper` error. Prefer arm64 goldens there.
- Some events need a payload supplied before they can be simulated.

## Relationship to clud-bosn

They do not overlap. bosn manages Docker resources on the **host**; a workflow run by
preloop uses the Docker engine **inside its own microVM**, which the host's bosn
neither sees nor tracks. Use bosn for the local build loop, preloop for the workflow.
