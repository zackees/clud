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
clud                              # Launch the configured backend in YOLO mode
clud --codex                      # Switch global backend to Codex
clud --claude                     # Switch global backend to Claude
clud --session-model=codex        # Use Codex for this run only
clud -c                           # Continue the most recent conversation
clud --resume                     # Open the resume picker
clud --resume abc123              # Resume a specific session / search term
clud --resume --codex             # Open the Codex resume picker
clud -p "refactor the auth layer" # Run with a prompt
clud -- --model opus              # Pass backend flags through
```

### Universal flags

These flags are handled by `clud` itself and work across supported backends.

- `-p`, `--prompt`: run with a prompt and exit when complete
- `-m`, `--message`: send a one-off message
- `--cmd`: execute a direct command without interactive mode
- `-c`, `--continue`: continue the most recent conversation
- `-r`, `--resume [TERM]`: resume by picker, session ID, or search term
- `--session-model <claude|codex>`: choose the backend for just this run
- `--claude`: persist Claude as the global backend
- `--codex`: persist Codex as the global backend
- `--model <NAME>`: set a backend-neutral model preference
- `--plain`: disable JSON formatting and use raw text I/O
- `-v`, `--verbose`, `--debug`: show debug output
- `--dry-run`: print what would run without executing it
- `--idle-timeout <SECONDS>`: auto-quit after idle detection
- `--hook-debug`: enable verbose hook logging
- `--no-hooks`: disable all hooks
- `--no-session-end-hook`: disable only the final `SessionEnd` hook
- `--no-stop-hook`: deprecated alias for `--no-session-end-hook`
- `--no-skills`: skip bundled skill auto-install
- `-h`, `--help`: show help

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

## Codex supported

![image (1)](https://github.com/user-attachments/assets/de1e23b4-4513-4c92-ba57-3d9dcd1060b6)

### Agent hook emulation

Codex now emulates the Claude-style repo hook lifecycle from `.claude/settings.json` and `.claude/settings.local.json`.

```json
{
  "hooks": {
    "Start": [...],
    "Stop": [...],
    "SessionEnd": [...]
  }
}
```

Hook names map like this:

| Config hook | Internal event | Meaning |
| --- | --- | --- |
| `Start` | `AGENT_START` | Agent session is starting |
| `Stop` | `POST_EXECUTION` | Agent finished a normal execution turn |
| `SessionEnd` | `AGENT_STOP` | Agent session is shutting down |

The important detail is that `Stop` is not the final shutdown hook. `SessionEnd` is the true end-of-session event.

You can control hook execution with:

- `--no-hooks` to disable all hooks
- `--no-session-end-hook` to disable only `SessionEnd`
- `--no-stop-hook` as a deprecated alias for `--no-session-end-hook`


## License

BSD 3-Clause License
