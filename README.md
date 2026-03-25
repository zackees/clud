# clud

![hero-clud](https://github.com/user-attachments/assets/4009dfee-e703-446d-b073-80d826708a10)

**A Python CLI wrapper for Claude Code that runs in YOLO mode by default — no permission prompts, maximum velocity.**

The name `clud` is simply a shorter, easier-to-type version of `claude`.

## Installation

```bash
pip install clud
```

Requires Claude Code installed separately (`npm install -g @anthropic-ai/claude-code@latest`).

## Quick Start

```bash
clud                              # Launch Claude Code in YOLO mode
clud -c                           # Continue previous conversation
clud --resume                     # Resume a specific conversation
clud -p "refactor the auth layer" # Run with a prompt
clud -- --model opus              # Pass any flags to Claude Code
```

## Features

### `clud loop` — Multi-Iteration Agent

Run Claude in an autonomous loop that iterates on a task until it's done (default: 50 iterations).

```bash
clud loop "Implement the API endpoints from the spec"
clud loop TASK.md
clud loop TASK.md --loop-count 10
```

Each iteration gets its own workspace in `.loop/` with task tracking, iteration summaries, and a `DONE.md` signal to halt early. The agent runs fully autonomously — no user interaction needed.

### `clud rebase` — Auto-Rebase

Fetches from origin and rebases the current branch, automatically resolving conflicts line-by-line.

```bash
clud rebase
```

### `clud up` — Project Maintenance

Runs lint, test, cleanup, then commits via the global `codeup` command.

```bash
clud up                    # Lint, test, and push
clud up -p                 # Publish to remote
clud up -m "commit msg"    # Custom commit message
```

### `-c` / `--continue` — Continue Conversation

```bash
clud -c
```

Continues the most recent Claude Code conversation.

### `--resume` — Resume Conversation

```bash
clud --resume
```

Resumes a specific Claude Code conversation (passed through to Claude Code).

### `--` — Passthrough

All unknown arguments are passed directly to Claude Code:

```bash
clud -- --model opus --verbose
clud -- --allowedTools "Bash(git*)"
```

## How It Works

When you run `clud`, it launches Claude Code with:

- `--dangerously-skip-permissions` — no safety prompts
- `CLAUDE_CODE_MAX_OUTPUT_TOKENS=64000` — max output for Sonnet
- Git co-author attribution disabled
- Pipe mode support (`echo "prompt" | clud | less`)

## Other Commands

```bash
clud fix [URL]             # Auto-fix linting + test failures
clud plan "prompt"         # Plan then auto-execute a task
clud --task PATH           # Execute a task file autonomously
clud --cron <subcommand>   # Schedule recurring tasks
clud --ui                  # Launch multi-terminal UI (4 xterm.js terminals)
clud --info                # Show Claude Code installation info
clud --install-claude      # Install Claude Code to ~/.clud/npm
clud --help                # Show all options
```

## Development

```bash
bash install               # Set up dev environment (requires uv)
source activate            # Activate virtualenv
bash test                  # Run unit tests
bash lint                  # Lint (mandatory after code changes)
```

## License

BSD 3-Clause License
