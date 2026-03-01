# clud

<!--
[![Windows Tests](https://github.com/zackees/clud/actions/workflows/windows-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-test.yml)[![macOS Tests](https://github.com/zackees/clud/actions/workflows/macos-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-test.yml)[![Linux Tests](https://github.com/zackees/clud/actions/workflows/linux-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-test.yml)
-->

![hero-clud](https://github.com/user-attachments/assets/4009dfee-e703-446d-b073-80d826708a10)

**A Python CLI that runs Claude Code in God mode by default, eliminating permission prompts and optimizing for long term agent work**

The name `clud` is simply a shorter, easier-to-type version of `claude`.

## Why is clud better?

Because safety is number three.



## Installation

```bash
pip install clud  # Everything included: CLI, service, and cluster control plane
```

## Quick Start

```bash
# Unleash Claude Code instantly (YOLO mode enabled by default)
clud

# Execute with specific prompt
clud -p "refactor this entire app"

# Send direct message
clud -m "add error handling"

# Pipe mode - seamless Unix-style integration
echo "make me a poem about roses" | clud
cat prompt.txt | clud | less
git log --oneline -5 | clud
```

## Pipe Mode (Unix & Windows)

**clud** supports full I/O piping for seamless integration with command-line workflows:

**Input Piping:**
```bash
# Pipe prompt from echo
echo "make me a poem about roses" | clud

# Pipe from file
cat prompt.txt | clud

# Pipe command output
git log --oneline -5 | clud
git diff | clud
```

**Output Piping:**
```bash
# Pipe to pager
clud -p "explain python asyncio" | less

# Pipe to file
clud -p "generate config" | tee config.json

# Pipe to grep
clud -p "list all functions" | grep "def "
```

**Chained Pipes:**
```bash
# Full pipeline
echo "summarize" | clud | cat
cat article.txt | clud | tee summary.txt | wc -w

# Complex workflows
git diff | clud -p "review this diff" | less
```

**Platform Support:**
- ✅ Linux, macOS (native bash/zsh)
- ✅ Windows (git-bash, MSYS2, WSL)
- ✅ Cross-platform TTY detection via `sys.stdin.isatty()`

## Operation Modes

### Default Mode

Launches Claude Code directly with YOLO mode enabled - no permission prompts, maximum velocity:

```bash
clud [directory]                    # Unleash Claude Code in directory
clud -p "refactor this entire app"  # Execute with specific prompt
clud -m "add error handling"        # Send direct message
clud --continue                     # Continue previous conversation
```

**Features:**
- Claude Code with dangerous permissions enabled by default
- Zero interruption workflow - no safety prompts
- Direct prompt execution for rapid iteration
- Supports all standard Claude Code arguments

### Advanced Modes

#### Fix Mode - Automated Linting & Testing
Automatically fix linting issues and run tests in a loop:

```bash
clud fix                          # Fix linting and run tests locally
clud fix https://github.com/...   # Fix issues from GitHub URL logs
```

Fix mode runs:
1. `lint-test` command up to 5 times, fixing issues each iteration

**Note:** `lint-test` is a safer alternative that wraps `codeup --lint --dry-run` and `codeup --test --dry-run` without exposing the command name to LLMs, preventing accidental bare `codeup` invocations.

#### Up Mode - Project Maintenance
Run the global `codeup` command with auto-fix capabilities:

```bash
clud up                           # Run codeup with auto-fix (up to 5 retries)
clud up -p                        # Run codeup -p for publishing
clud up --publish                 # Same as above
```

### Task System

Clud includes a powerful task management system for structured development workflows:

```bash
clud --task task.md               # Open task file and execute autonomously
```

**Task Workflow:**
- Opens task file in editor for review/editing
- Executes task autonomously using research-plan-implement-test-fix-lint cycle
- Continues until completion, needs feedback, or reaches 50 iterations
- Provides final summary with status (SUCCESS, NEED FEEDBACK, or NOT DONE)

### Multi-Terminal UI

Launch a Playwright browser with 4 interactive terminals in a grid layout:

```bash
clud --ui                         # Launch 4-terminal UI
clud -d                           # Short alias
```

**Features:**
- 4 xterm.js terminals in a 2-column grid layout
- All terminals start in your home directory
- Full shell access with ANSI color support
- Cross-platform (Windows git-bash, Unix/Linux bash/zsh)
- Run `clud --loop` or `clud --cron` from any terminal
- VS Code Dark+ color theme
- Auto-resize on window resize
- Clean shutdown when browser is closed

**Use Case:**
The daemon provides a convenient way to run multiple `clud` instances simultaneously, each in its own terminal. Perfect for parallel development workflows or monitoring multiple projects.

### Hook System

Clud includes an event-based architecture for intercepting execution events:

**Hook System (`src/clud/hooks/`):**
- Event-based interception: PRE_EXECUTION, POST_EXECUTION, OUTPUT_CHUNK, ERROR, AGENT_START, AGENT_STOP
- Built-in handlers: WebhookHandler
- Configuration via .clud files and environment variables

## Command Reference

### Main Commands
```bash
clud [directory]                  # Launch in directory
clud -p "prompt"                  # Execute with prompt
clud -m "message"                 # Send message
clud --continue                   # Continue conversation
clud --dry-run                    # Show command without executing
```

### Utility Commands
```bash
clud --task PATH                  # Process task file
clud --lint                       # Run linting workflow
clud --test                       # Run testing workflow
clud fix [URL]                    # Auto-detect and fix linting + tests
clud --ui                         # Launch 4-terminal UI
clud -d                           # Short alias for --ui
clud --help                       # Show help
```

### Quick Mode Aliases
```bash
clud fix [URL]                    # Fix linting and test issues
clud up [-p|--publish]            # Run global codeup command with auto-fix
```

## Development

### Setup Development Environment

```bash
# Install dependencies (requires uv)
bash install

# Activate virtual environment
source activate

# Run tests
bash test

# Run linting
bash lint
```

### Project Structure

```
clud/
├── src/clud/              # Main package source
│   ├── cli.py            # Minimal CLI router (delegates to agent_cli)
│   ├── agent_cli.py      # Consolidated agent execution module
│   ├── agent_args.py     # Unified argument parser
│   ├── agent/            # Agent support subpackage
│   │   ├── foreground.py      # Direct Claude Code execution
│   │   ├── foreground_args.py # Argument parsing
│   │   └── completion.py      # Completion detection
│   ├── hooks/            # Hook system for event interception
│   │   ├── __init__.py        # HookManager, HookEvent, HookContext
│   │   ├── webhook.py         # WebhookHandler
│   │   └── config.py          # Configuration loading
│   ├── daemon/           # Multi-terminal daemon (Playwright)
│   │   ├── __init__.py        # Daemon proxy class
│   │   ├── playwright_daemon.py  # Main daemon orchestrator
│   │   ├── server.py          # HTTP + WebSocket server
│   │   ├── terminal_manager.py   # PTY session management
│   │   ├── html_template.py   # xterm.js grid template
│   │   └── cli_handler.py     # CLI handler for --ui
│   ├── cron/             # Cron scheduler
│   │   ├── __init__.py        # Cron proxy class
│   │   ├── daemon.py          # Background daemon
│   │   └── ...
│   ├── task.py           # Task management system
│   └── ...
├── tests/                # Unit and integration tests
│   ├── test_daemon.py         # Daemon unit tests (33 test methods)
│   ├── integration/           # E2E tests
│   │   └── test_daemon_e2e.py # Daemon E2E tests
│   └── ...
├── pyproject.toml        # Package configuration
└── ...
```

### Requirements

- **Python 3.13+** (uses modern Python features)
- **uv** (package manager) - https://docs.astral.sh/uv/
- **git-bash** (on Windows)

### Windows Development

This project is designed to work on Windows using `git-bash` for proper Unix-like shell support. UTF-8 encoding is handled automatically in all shell scripts.

### Testing

```bash
# Run all tests
bash test

# Run with verbose output
bash test -v

# Run integration tests (sequential, avoid resource conflicts)
uv run pytest tests/integration/ -v --tb=short --maxfail=1

# Run specific test file
bash test tests/test_task.py
```

### Code Quality

```bash
# Run full linting suite
bash lint

# Run ruff check with auto-fix
uv run ruff check --fix src/ tests/

# Run ruff format
uv run ruff format src/ tests/

# Run type checking
uv run pyright
```

## Entry Points

The package provides a single CLI entry point:

- `clud` - Main CLI (runs Claude Code in YOLO mode)

## Cool Projects

### Curated Lists & Frameworks
- **Awesome Claude Code**: https://github.com/hesreallyhim/awesome-claude-code - Curated list of commands, files, and workflows
- **SuperClaude Framework**: https://github.com/SuperClaude-Org/SuperClaude_Framework - Framework for building Claude Code applications
- **AB Method**: https://github.com/ayoubben18/ab-method - Workflow methodology for Claude Code

### Web UIs
- **Claude Code WebUI** (sugyan): https://github.com/sugyan/claude-code-webui - Web interface for Claude Code
- **ClaudeCodeUI**: https://github.com/siteboon/claudecodeui - Alternative web UI for Claude Code

### Official Tools
- **Claude Code GitHub Action**: https://github.com/anthropics/claude-code-action - AI-powered automation for GitHub PRs and issues

### IDE Integrations
- **claude-code.nvim**: Neovim integration for Claude Code
- **claude-code-ide.el**: Emacs integration with ediff-based code suggestions

### Monitoring & Management
- **CC Usage**: CLI tool for managing and analyzing Claude Code usage and costs
- **ccflare**: Claude Code usage dashboard with web UI

### Workflow Tools
- **Crystal**: Desktop application for orchestrating and monitoring Claude Code agents
- **Happy Coder**: Spawn and control multiple Claude Codes in parallel with push notifications
- **Claude Task Runner**: Manages context isolation and focused task execution
- **Backlog.md**: Tool for managing project collaboration between humans and AI agents in a git ecosystem - turns any folder with a Git repo into a self-contained project board using plain Markdown files with zero-config CLI

## TODO / Roadmap

- [ ] **Skills.sh Integration** - Integrate https://skills.sh/ functionality into clud

## License

BSD 3-Clause License

## Contributing

Contributions are welcome! Please ensure all tests pass and code is properly linted before submitting PRs.


```bash
# Setup environment
bash install

# Run tests
bash test

# Lint code
bash lint
```

# Is `clud --loop` AGI?



<img width="556" height="500" alt="image" src="https://github.com/user-attachments/assets/520f6131-5409-4b29-927a-2b946c4ecb79" />

**Yes.**
