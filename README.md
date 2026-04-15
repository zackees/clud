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
clud                              # Launch Claude in YOLO mode via subprocess
clud --codex                      # Use Codex as the backend
clud --claude                     # Use Claude as the backend (default)
clud --pty                        # Force PTY launch mode
clud --subprocess                 # Force subprocess launch mode
clud --detach -p "review this PR" # Start a daemon-managed session without attaching
clud --detachable -p "fix CI"     # Ctrl+C detaches; reattach later with clud attach
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
clud attach                       # List attachable daemon-managed sessions
clud attach sess-123              # Attach to a specific session
clud list                         # Show attachable session IDs, PIDs, and cwd
clud wasm guest.wasm              # Run a local wasm module with clud's embedded runtime
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
| `--subprocess` | Force subprocess launch mode |
| `--pty` | Force PTY launch mode |
| `--detach` | Start a daemon-managed session and exit after printing the session ID |
| `--detachable` | Run attached under the daemon; `Ctrl+C` detaches instead of interrupting |
| `--model <NAME>` | Set model preference (e.g., haiku, sonnet, opus) |
| `--safe` | Disable YOLO mode (don't inject `--dangerously-skip-permissions`) |
| `--dry-run` | Print what would be executed, then exit |
| `-v`, `--verbose` | Show debug output |
| `-h`, `--help` | Show help |
| `-V`, `--version` | Show version |

Unknown flags are forwarded directly to the backend agent.

`clud` now defaults to subprocess launch mode for Claude and Codex. Use `--pty`
to opt back into PTY while Claude PTY issues are being investigated.

## Detached Sessions

Use daemon-managed sessions when you want to disconnect and reattach later.

```bash
clud --detachable --codex -p "refactor the parser"
# press Ctrl+C to detach

clud attach
clud attach sess-123
clud list
```

`clud attach` without a session ID lists live attachable sessions. `clud list`
shows the same sessions with their root PID and current working directory.

## Voice Mode

`clud` can capture microphone input and transcribe it into the active console prompt with
`whisper-rs`.

```bash
# English-only small model
export CLUD_VOICE=1
export CLUD_WHISPER_MODEL=/path/to/ggml-small.en.bin

clud
```

On Windows PowerShell:

```powershell
$env:CLUD_VOICE = "1"
$env:CLUD_WHISPER_MODEL = "C:\models\ggml-small.en.bin"

clud
```

Behavior:

- Press `F3` to start recording and play a short `ding`
- Release `F3` to stop recording and play a short `dong`
- The transcript is inserted into the current prompt without auto-submitting it
- If the terminal does not emit key-release events, pressing `F3` again stops recording

Optional environment variables:

| Variable | Description |
|----------|-------------|
| `CLUD_VOICE` | Enable voice mode (`1`, `true`, `yes`, `on`) |
| `CLUD_WHISPER_MODEL` | Path to a local `whisper.cpp` GGML model such as `ggml-small.en.bin` |
| `CLUD_VOICE_LANGUAGE` | Force a Whisper language code such as `en` |

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

## `clud wasm` â€” Embedded Runtime

Loads a local `.wasm` module, wires up a host logging import, and invokes an exported function.

```bash
clud wasm hello.wasm
clud wasm hello.wasm --invoke _start
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
