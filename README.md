# clud

![hero-clud](https://github.com/user-attachments/assets/4009dfee-e703-446d-b073-80d826708a10)

**A Python CLI wrapper for Claude Code that runs in YOLO mode by default — no permission prompts, maximum velocity.**

The name `clud` is simply a shorter, easier-to-type version of `claude`.

## Installation

```bash
pip install clud
```

## Usage

```bash
clud                              # Launch Claude Code in YOLO mode
clud -c                           # Continue previous conversation
clud --resume                     # Resume a specific conversation
clud -p "refactor the auth layer" # Run with a prompt
clud -- --model opus              # Pass any flags to Claude Code
```

## `clud loop` — The Ralph Loop

![image](https://github.com/user-attachments/assets/b6666429-ead7-419c-831f-db4e17b3840b)

Run Claude in an autonomous loop that iterates on a task until it's done (default: 50 iterations).

```bash
clud loop "Implement the API endpoints from the spec"
clud loop TASK.md
clud loop TASK.md --loop-count 10
```

Each iteration gets its own workspace in `.loop/` with task tracking, iteration summaries, and a `DONE.md` signal to halt early. Fully autonomous — no user interaction needed.

## `clud rebase` — Auto-Rebase

Did changes happen on origin? This syncs it up — fetches from origin, rebases the current branch, and resolves conflicts line-by-line without reverts.

```bash
clud rebase
```

## `clud fix` — Auto-Fix

Auto-detects linting and test tools in your repo, runs them, and fixes failures in a loop until everything passes.

```bash
clud fix                   # Detect and fix lint + test issues
clud fix https://github.com/user/repo/actions/runs/123  # Fix from CI logs
```

## `clud up` — Ship It

Runs lint, test, cleanup, then commits via `codeup`.

```bash
clud up                    # Lint, test, and push
clud up -p                 # Publish to remote
clud up -m "commit msg"    # Custom commit message
```

## License

BSD 3-Clause License
