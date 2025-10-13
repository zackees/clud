# clud

[![Windows Tests](https://github.com/zackees/clud/actions/workflows/windows-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-test.yml)[![macOS Tests](https://github.com/zackees/clud/actions/workflows/macos-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-test.yml)[![Linux Tests](https://github.com/zackees/clud/actions/workflows/linux-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-test.yml)

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
```

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
1. `codeup --lint --dry-run` up to 5 times, fixing issues each iteration
2. `codeup --test --dry-run` up to 5 times, fixing issues each iteration
3. Final lint pass to ensure code quality

#### Up Mode - Project Maintenance
Run the global `codeup` command with auto-fix capabilities:

```bash
clud up                           # Run codeup with auto-fix (up to 5 retries)
clud --codeup-publish            # Run codeup -p for publishing
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

### Kanban Board

Launch an interactive kanban board for task management:

```bash
clud --kanban                     # Launch vibe-kanban board
```

### Cluster Control Plane

`clud` now includes a cluster control plane for monitoring and managing agents across your development environment:

```bash
clud-cluster serve                # Start cluster control plane (default port :8000)
clud-cluster serve --host 0.0.0.0 --port 9000  # Custom host/port
clud-cluster migrate              # Run database migrations
clud-cluster bot                  # Run Telegram bot (requires bot extra)
```

**Features:**
- Web-based UI for agent monitoring and management
- Real-time WebSocket updates for agent status
- JWT authentication for secure access
- SQLite database for persistent storage
- Telegram bot integration for notifications
- RESTful API for programmatic access

**Port Assignments:**
- Background service: `:7565`
- Cluster control plane: `:8000` (default, configurable)

**Architecture:**
The cluster control plane provides a unified interface for monitoring all `clud` agents running on your system. Agents register with the background service (`:7565`), which communicates with the cluster control plane to provide real-time status updates, metrics, and control capabilities.

## Configuration

### API Key Setup

```bash
# Interactive setup (recommended)
clud --login

# Use environment variable
export ANTHROPIC_API_KEY="sk-ant-..."

# Use command line
clud --api-key "sk-ant-..."
```

The API key is stored securely in `~/.clud/anthropic-api-key.key` for future use.



## Command Reference

### Main Commands
```bash
clud [directory]                  # Launch in directory
clud -p "prompt"                  # Execute with prompt
clud -m "message"                 # Send message
clud --continue                   # Continue conversation
clud --api-key KEY                # Specify API key
clud --dry-run                    # Show command without executing
```

### Utility Commands
```bash
clud --login                      # Configure API key
clud --task PATH                  # Process task file
clud --lint                       # Run linting workflow
clud --test                       # Run testing workflow
clud --fix [URL]                  # Fix linting and tests
clud --codeup                     # Run codeup with auto-fix
clud --codeup-publish             # Run codeup -p
clud --kanban                     # Launch kanban board
clud --help                       # Show help
```

### Cluster Control Plane Commands
```bash
clud-cluster serve                # Start cluster control plane
clud-cluster serve --host HOST    # Specify host
clud-cluster serve --port PORT    # Specify port
clud-cluster serve --reload       # Enable auto-reload
clud-cluster migrate              # Run database migrations
clud-cluster bot                  # Run Telegram bot
clud-cluster --help               # Show help
```

### Quick Mode Aliases
```bash
clud fix                          # Alias for --fix
clud up                           # Alias for --codeup
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
│   ├── cli.py            # CLI router and main entry point
│   ├── agent/            # Agent execution subpackage
│   │   ├── foreground.py      # Direct Claude Code execution
│   │   ├── foreground_args.py # Argument parsing
│   │   ├── completion.py      # Completion detection
│   │   └── tracking.py        # Agent tracking
│   ├── service/          # Background service (formerly daemon)
│   │   ├── server.py          # HTTP server on :7565
│   │   ├── registry.py        # Agent registry
│   │   ├── models.py          # Shared models
│   │   └── central_client.py  # Connects to central
│   ├── central/          # Cluster control plane
│   │   ├── app.py            # FastAPI application
│   │   ├── cli.py            # Central CLI entry point
│   │   ├── auth.py           # JWT authentication
│   │   ├── database.py       # SQLAlchemy models
│   │   ├── telegram_bot.py   # Telegram integration
│   │   ├── websocket_handlers.py # WebSocket protocol
│   │   └── static/           # Built React UI
│   ├── task.py           # Task management system
│   └── ...
├── packages/web/         # React frontend source
├── tests/                # Unit and integration tests
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

The package provides two CLI entry points:

- `clud` - Main CLI (runs Claude Code in YOLO mode)
- `clud-cluster` - Cluster control plane for monitoring and managing agents

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

Yes.

<img width="556" height="500" alt="image" src="https://github.com/user-attachments/assets/520f6131-5409-4b29-927a-2b946c4ecb79" />
