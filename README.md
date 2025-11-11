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

### Kanban Board

Launch an interactive kanban board for task management:

```bash
clud --kanban                     # Launch vibe-kanban board
```

### Web UI

Launch a browser-based interface for Claude Code with real-time streaming:

```bash
clud --webui                      # Launch Web UI on port 8888 (auto-opens browser)
clud --webui 3000                 # Launch on custom port 3000
```

**Features:**
- Real-time streaming chat interface with Claude Code
- **Integrated terminal console** with split-pane layout and xterm.js
  - Multiple terminals with tabbed interface
  - Full shell access with ANSI color support
  - Cross-platform (Windows git-bash/cmd, Unix/Linux bash/zsh)
  - Adjustable resize handle between chat and terminal panels
- Project directory selection
- Conversation history (last 10 messages loaded on startup)
- Dark/light theme toggle
- Mobile-responsive design
- WebSocket-based streaming for instant responses
- Runs in YOLO mode (no permission prompts)
- Markdown rendering with code block syntax highlighting

**Architecture:**
The Web UI is a FastAPI-based server that wraps Claude Code execution and provides a clean chat interface. It uses WebSocket for real-time streaming of Claude's responses and stores conversation history in browser localStorage. The integrated terminal uses PTY (pseudo-terminal) for full shell access with cross-platform support.

**Security Note:** The Web UI includes full shell access through the integrated terminal. Only run on trusted localhost environments. Network deployment requires authentication and security hardening.

**Inspired by:** [sugyan/claude-code-webui](https://github.com/sugyan/claude-code-webui) - Python/FastAPI implementation with minimal dependencies.

### Advanced Telegram Integration

Interact with Claude Code through Telegram with a synchronized web dashboard for monitoring conversations:

```bash
clud --telegram-server                    # Launch server on default port 8889
clud --telegram-server 9000               # Launch on custom port
clud --telegram-server --telegram-config telegram_config.yaml  # Use config file
```

**Features:**
- **Telegram Bot Integration**: Send messages to your bot, get responses from Claude Code
- **Real-time Web Dashboard**: Monitor all Telegram conversations in a web interface
- **Multi-Session Support**: Handle multiple concurrent users with isolated sessions
- **Message History**: Full conversation history synchronized between Telegram and web
- **WebSocket Streaming**: Real-time updates with minimal latency
- **SvelteKit Frontend**: Modern, responsive UI with dark/light theme support
- **Session Management**: View active sessions, switch between users, monitor activity

**Quick Setup:**
1. Get a bot token from [@BotFather](https://t.me/BotFather) on Telegram
2. Set your bot token: `export TELEGRAM_BOT_TOKEN="your_token_here"`
3. Start the server: `clud --telegram-server`
4. Message your bot on Telegram and watch responses appear on both Telegram and the web dashboard

**Configuration:**
- Environment variables: `TELEGRAM_BOT_TOKEN`, `TELEGRAM_WEB_PORT`, `TELEGRAM_ALLOWED_USERS`
- Configuration file: See `telegram_config.example.yaml` for all options
- Example environment file: `.env.example`

**Documentation:**
- Full guide: [docs/telegram-integration.md](docs/telegram-integration.md)
- API reference, troubleshooting, security considerations, and more

**Architecture:**
The integration uses a SessionManager to orchestrate message flow between Telegram, Claude Code instances (via InstancePool), and web clients (via WebSocket). Each Telegram user gets their own isolated session with persistent message history.

### Hook System & Message Handler API

Clud includes a sophisticated event-based architecture for intercepting and forwarding execution events to external systems:

**Hook System (`src/clud/hooks/`):**
- Event-based interception: PRE_EXECUTION, POST_EXECUTION, OUTPUT_CHUNK, ERROR, AGENT_START, AGENT_STOP
- Built-in handlers: TelegramHookHandler, WebhookHandler
- Configuration via .clud files and environment variables

**Message Handler API (`src/clud/api/`):**
- Unified API for routing messages from multiple client types to clud instances
- Instance pooling with session-based reuse and automatic cleanup
- WebSocket streaming for real-time output
- REST endpoints: `/api/message`, `/api/instances`, health checks
- Supports concurrent sessions with configurable limits (default: 100 instances, 30-minute idle timeout)

This architecture enables:
- Telegram native clients can send messages to running clud instances with real-time output streaming
- Web clients can interact via REST API with the same functionality
- Multiple users can interact with separate clud instances simultaneously
- Sessions persist across multiple messages in the same chat

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
clud --kanban                     # Launch kanban board
clud --webui [PORT]               # Launch Web UI (default port: 8888)
clud --telegram-server [PORT]     # Launch Telegram integration server (default port: 8889)
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
│   │   ├── foreground.py      # [Legacy] Direct Claude Code execution
│   │   ├── foreground_args.py # [Legacy] Argument parsing
│   │   ├── completion.py      # Completion detection
│   │   └── tracking.py        # Agent tracking
│   ├── hooks/            # Hook system for event interception
│   │   ├── __init__.py        # HookManager, HookEvent, HookContext
│   │   ├── telegram.py        # TelegramHookHandler
│   │   ├── webhook.py         # WebhookHandler
│   │   └── config.py          # Configuration loading
│   ├── api/              # Message Handler API
│   │   ├── models.py          # Data models (MessageRequest, MessageResponse)
│   │   ├── message_handler.py # Core routing logic
│   │   ├── instance_manager.py # Instance lifecycle management
│   │   └── server.py          # FastAPI server
│   ├── webui/            # Web UI for browser-based interface
│   │   ├── server.py          # FastAPI application
│   │   ├── api.py             # Handler classes
│   │   ├── pty_manager.py     # Cross-platform PTY session management
│   │   ├── terminal_handler.py # WebSocket handler for terminal I/O
│   │   └── static/            # HTML/CSS/JavaScript frontend (xterm.js)
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
│   ├── test_hooks.py          # Hook system tests (17 test methods)
│   ├── test_api_models.py     # API models tests (25 test methods)
│   ├── test_message_handler.py # Message handler tests (13 test methods)
│   ├── test_instance_manager.py # Instance manager tests (23 test methods)
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
- **Backlog.md**: Tool for managing project collaboration between humans and AI agents in a git ecosystem - turns any folder with a Git repo into a self-contained project board using plain Markdown files with zero-config CLI

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
