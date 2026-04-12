# clud

![hero-clud](https://github.com/user-attachments/assets/4009dfee-e703-446d-b073-80d826708a10)

**A fast Rust CLI for Claude Code and Codex that runs in YOLO mode by default — no permission prompts, maximum velocity.**

The name `clud` is simply a shorter, easier-to-type version of `claude`.

| Platform | Build | Lint | Unit Test | Integration Test |
|----------|-------|------|-----------|------------------|
| Linux x86 | [![Build](https://github.com/zackees/clud/actions/workflows/linux-x86-build.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-x86-build.yml) | [![Lint](https://github.com/zackees/clud/actions/workflows/linux-x86-lint.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-x86-lint.yml) | [![Unit Test](https://github.com/zackees/clud/actions/workflows/linux-x86-unit-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-x86-unit-test.yml) | [![Integration Test](https://github.com/zackees/clud/actions/workflows/linux-x86-integration-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-x86-integration-test.yml) |
| Linux ARM | [![Build](https://github.com/zackees/clud/actions/workflows/linux-arm-build.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-arm-build.yml) | [![Lint](https://github.com/zackees/clud/actions/workflows/linux-arm-lint.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-arm-lint.yml) | [![Unit Test](https://github.com/zackees/clud/actions/workflows/linux-arm-unit-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-arm-unit-test.yml) | [![Integration Test](https://github.com/zackees/clud/actions/workflows/linux-arm-integration-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-arm-integration-test.yml) |
| Windows x86 | [![Build](https://github.com/zackees/clud/actions/workflows/windows-x86-build.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-x86-build.yml) | [![Lint](https://github.com/zackees/clud/actions/workflows/windows-x86-lint.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-x86-lint.yml) | [![Unit Test](https://github.com/zackees/clud/actions/workflows/windows-x86-unit-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-x86-unit-test.yml) | [![Integration Test](https://github.com/zackees/clud/actions/workflows/windows-x86-integration-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-x86-integration-test.yml) |
| Windows ARM | [![Build](https://github.com/zackees/clud/actions/workflows/windows-arm-build.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-arm-build.yml) | [![Lint](https://github.com/zackees/clud/actions/workflows/windows-arm-lint.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-arm-lint.yml) | [![Unit Test](https://github.com/zackees/clud/actions/workflows/windows-arm-unit-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-arm-unit-test.yml) | [![Integration Test](https://github.com/zackees/clud/actions/workflows/windows-arm-integration-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-arm-integration-test.yml) |
| macOS x86 | [![Build](https://github.com/zackees/clud/actions/workflows/macos-x86-build.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-x86-build.yml) | [![Lint](https://github.com/zackees/clud/actions/workflows/macos-x86-lint.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-x86-lint.yml) | [![Unit Test](https://github.com/zackees/clud/actions/workflows/macos-x86-unit-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-x86-unit-test.yml) | [![Integration Test](https://github.com/zackees/clud/actions/workflows/macos-x86-integration-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-x86-integration-test.yml) |
| macOS ARM | [![Build](https://github.com/zackees/clud/actions/workflows/macos-arm-build.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-arm-build.yml) | [![Lint](https://github.com/zackees/clud/actions/workflows/macos-arm-lint.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-arm-lint.yml) | [![Unit Test](https://github.com/zackees/clud/actions/workflows/macos-arm-unit-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-arm-unit-test.yml) | [![Integration Test](https://github.com/zackees/clud/actions/workflows/macos-arm-integration-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-arm-integration-test.yml) |

## Installation

```bash
pip install clud
```

## Usage

```bash
clud                              # Launch Claude in YOLO mode (interactive)
clud --codex                      # Use Codex as the backend
clud --claude                     # Use Claude as the backend (default)
clud -c                           # Continue the most recent conversation
clud --resume                     # Resume a session
clud --resume abc123              # Resume a specific session by ID or search term
clud -p "refactor the auth layer" # Run with a prompt, exit when done
clud -m "what does this do?"      # Send a one-off message
clud --model opus -p "review PR"  # Choose a model
clud --safe -p "drop the table"   # Disable YOLO mode (keeps permission prompts)
clud --dry-run -p "hello"         # Print what would run without executing
echo "explain this error" | clud  # Pipe mode: read prompt from stdin
clud -- --verbose --debug         # Pass extra flags through to the backend
```

### Flags

| Flag | Description |
|------|-------------|
| `-p`, `--prompt` | Run with a prompt, exit when complete |
| `-m`, `--message` | Send a one-off message |
| `-c`, `--continue` | Continue the most recent conversation |
| `-r`, `--resume [TERM]` | Resume by session ID or search term |
| `--claude` | Use Claude as the backend |
| `--codex` | Use Codex as the backend |
| `--model <NAME>` | Set model preference (e.g., haiku, sonnet, opus) |
| `--safe` | Disable YOLO mode (don't inject `--dangerously-skip-permissions`) |
| `--dry-run` | Print what would be executed, then exit |
| `-v`, `--verbose` | Show debug output |
| `-h`, `--help` | Show help |
| `-V`, `--version` | Show version |

Unknown flags are forwarded directly to the backend agent.

## `clud loop` — Autonomous Loop

Run the backend in an autonomous loop that iterates on a task (default: 50 iterations).

```bash
clud loop "Implement the API endpoints from the spec"
clud loop TASK.md                     # Read prompt from a file
clud loop --loop-count 10 "fix bugs"  # Custom iteration count
```

The loop stops early if any iteration exits with a non-zero code.

## `clud rebase` — Auto-Rebase

Fetches from origin, rebases the current branch, and resolves conflicts.

```bash
clud rebase
```

## `clud fix` — Auto-Fix

Detects linting and test tools in your repo, runs them, and fixes failures in a loop until everything passes.

```bash
clud fix
```

## `clud up` — Ship It

Runs lint, test, cleanup, then commits.

```bash
clud up
```

## Development

```bash
bash build                  # Build dev wheel (Rust binary + Python package)
bash lint                   # Lint (cargo fmt + clippy + ruff + banned imports)
bash test                   # Unit tests (Rust + Python)
bash test --integration     # Include integration tests with mock agents
```

## License

BSD 3-Clause License
