# clud

Claude, but on god mode, by default.

[![Build and Push Multi Docker Image](https://github.com/zackees/clud/actions/workflows/build_multi_docker_image.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/build_multi_docker_image.yml)[![Windows Tests](https://github.com/zackees/clud/actions/workflows/windows-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-test.yml)[![macOS Tests](https://github.com/zackees/clud/actions/workflows/macos-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-test.yml)[![Linux Tests](https://github.com/zackees/clud/actions/workflows/linux-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-test.yml)
[![Integration Tests](https://github.com/zackees/clud/actions/workflows/integration-tests.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/integration-tests.yml)

![hero-clud](https://github.com/user-attachments/assets/4009dfee-e703-446d-b073-80d826708a10)

**A Python CLI that runs Claude Code in "YOLO mode" by default, eliminating permission prompts for maximum development velocity**. In other words, it unlocks god mode for claude cli.

The name `clud` is simply a shorter, easier-to-type version of `claude`. Use `bg` mode to launch the sandboxed background agent with full Docker server capabilities.

## Why is clud better?

Claude Code's safety prompts, while well-intentioned, slow down experienced developers. `clud` removes these friction points by running Claude Code with `--dangerously-skip-permissions` in both foreground and background modes, delivering the uninterrupted coding experience Claude Code was meant to provide.

## Installation

```bash
# Basic installation
pip install clud

# With messaging support (Telegram, SMS, WhatsApp notifications)
pip install clud[messaging]

# Full installation (all features)
pip install clud[full]
```

## Quick Start

```bash
# Unleash Claude Code instantly (YOLO mode enabled by default)
clud

# Get real-time updates via Telegram/SMS/WhatsApp
clud --notify-user "@yourusername" -m "Fix authentication bug"
clud --notify-user "+14155551234" -m "Deploy to production"
clud --notify-user "whatsapp:+14155551234" -m "Run all tests"

# Launch background agent with full Docker server capabilities
clud bg

# Launch background agent with web UI for browser-based development
clud bg --open

# Execute specific commands in isolated container
clud bg --cmd "python test.py"
```

## Operation Modes

### Foreground Agent (Default)

Launches Claude Code directly with YOLO mode enabled - no permission prompts, maximum velocity:

```bash
clud [directory]                    # Unleash Claude Code in directory
clud -p "refactor this entire app"  # Execute with specific prompt
clud -m "add error handling"        # Send direct message
clud --continue                     # Continue previous conversation

# With real-time notifications
clud --notify-user "@username" -m "Deploy app"    # Telegram
clud --notify-user "+14155551234" -m "Run tests"  # SMS
clud --notify-user "whatsapp:+1234567890" -m "Build" # WhatsApp
```

**Features:**
- Claude Code with dangerous permissions enabled by default
- Zero interruption workflow - no safety prompts
- Direct prompt execution for rapid iteration
- Real-time status updates via Telegram/SMS/WhatsApp
- Supports all standard Claude Code arguments

### Background Agent (`bg` mode)

Full Docker server mode with the same YOLO approach - perfect for complex development workflows:

```bash
clud bg [directory]               # Launch container shell (YOLO enabled)
clud bg --open                    # Launch VS Code server in browser
clud bg --cmd "pytest tests/"     # Run specific commands in container
clud bg --ssh-keys                # Mount SSH keys for git operations
clud bg --build-dockerfile PATH   # Build custom Docker image
```

**Features:**
- Interactive container shell with YOLO Claude Code built-in
- VS Code server (code-server) accessible via browser on port 8743
- Isolated `/workspace` directory synced from host `/host`
- SSH key mounting for seamless git operations
- Custom Docker image building and management
- Git worktree support for isolated branch development
- Container sync mechanism for workspace isolation

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

### Messaging Setup (Optional)

Get real-time status updates via Telegram, SMS, or WhatsApp:

```bash
# Interactive setup (recommended)
clud --configure-messaging

# Or use environment variables
export TELEGRAM_BOT_TOKEN="1234567890:ABC..."
export TWILIO_ACCOUNT_SID="ACxxxxxxxxxx"
export TWILIO_AUTH_TOKEN="your_token"
export TWILIO_FROM_NUMBER="+15555555555"
```

**See [MESSAGING_SETUP.md](MESSAGING_SETUP.md) for detailed setup instructions.**

#### Quick Setup:

**Telegram (Free, Recommended):**
1. Create bot: Message @BotFather on Telegram → `/newbot`
2. Get chat ID: Message @userinfobot
3. Configure: `clud --configure-messaging`
4. Use: `clud --notify-user "123456789" -m "task"`

**SMS/WhatsApp (via Twilio):**
1. Sign up: https://www.twilio.com/try-twilio (get $15 free credit)
2. Get phone number and credentials
3. Configure: `clud --configure-messaging`
4. Use: `clud --notify-user "+14155551234" -m "task"` (SMS)
5. Use: `clud --notify-user "whatsapp:+14155551234" -m "task"` (WhatsApp)

### Container Configuration

Configure container behavior with `.clud` file in your project root:

```json
{
  "dockerfile": "path/to/custom/Dockerfile"
}
```

## Background Mode Features

### Container Shell Options

```bash
# Basic options
clud bg --port 8080               # Custom port for code-server
clud bg --cmd "bash"              # Custom command to run
clud bg --shell zsh               # Use different shell

# Security options
clud bg --ssh-keys                # Mount SSH keys (read-only)
clud bg --read-only-home          # Mount home as read-only
clud bg --no-sudo                 # Disable sudo in container
clud bg --no-firewall             # Disable container firewall

# Development options
clud bg --claude-commands PATH    # Mount custom Claude CLI plugins
clud bg --env KEY=VALUE           # Pass environment variables
clud bg --detect-completion       # Enable idle detection (3s timeout)
clud bg --open                    # Open VS Code server in browser
```

### Git Worktree Support

Clud supports Git worktrees for isolated branch development inside containers:

```bash
# Worktrees are automatically available in /workspace
# Host repository is mounted at /host
```

The container sync mechanism (`sync` command inside container) handles bidirectional synchronization between `/host` and `/workspace`.

## Docker Image

The Docker image is available on Docker Hub as: **`niteris/clud`**

**Image includes:**
- Ubuntu 24.04 base
- Python 3.13
- Node.js 22 (via fnm)
- Claude CLI with YOLO mode alias
- Code-server for web-based development
- Essential dev tools (git, vim, ripgrep, fzf, lazygit, etc.)
- MCP server support (@modelcontextprotocol/server-filesystem)

## Command Reference

### Foreground Mode Commands
```bash
clud [directory]                  # Launch in directory
clud -p "prompt"                  # Execute with prompt
clud -m "message"                 # Send message
clud --continue                   # Continue conversation
clud --api-key KEY                # Specify API key
clud --dry-run                    # Show command without executing
```

### Background Mode Commands
```bash
clud bg [directory]               # Launch container
clud bg --cmd "command"           # Execute command
clud bg --open                    # Open VS Code server
clud bg --ssh-keys                # Mount SSH keys
clud bg --build-dockerfile PATH   # Build custom image
clud bg --port PORT               # Custom port
clud bg --detect-completion       # Enable completion detection
```

### Utility Commands
```bash
clud --login                      # Configure API key
clud --configure-messaging        # Configure Telegram/SMS/WhatsApp
clud --task PATH                  # Process task file
clud --lint                       # Run linting workflow
clud --test                       # Run testing workflow
clud --fix [URL]                  # Fix linting and tests
clud --codeup                     # Run codeup with auto-fix
clud --codeup-publish             # Run codeup -p
clud --kanban                     # Launch kanban board
clud --help                       # Show help
```

### Notification Commands
```bash
# Telegram
clud --notify-user "@username" -m "task"          # Telegram username
clud --notify-user "123456789" -m "task"          # Telegram chat ID
clud --notify-user "telegram:@user" -m "task"     # Explicit prefix

# SMS
clud --notify-user "+14155551234" -m "task"       # Phone number

# WhatsApp
clud --notify-user "whatsapp:+14155551234" -m "task"

# Custom update interval (seconds)
clud --notify-user "@user" --notify-interval 60 -m "task"
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

# Build Docker image
docker build -t niteris/clud .
```

### Project Structure

```
clud/
├── src/clud/              # Main package source
│   ├── cli.py            # CLI router and main entry point
│   ├── agent_foreground.py   # Foreground YOLO mode agent
│   ├── agent_background.py   # Background Docker agent
│   ├── task.py           # Task management system
│   ├── docker/           # Docker utilities
│   │   └── docker_manager.py
│   ├── git_worktree.py   # Git worktree support
│   └── ...
├── docker/               # Docker-related files
│   └── container_sync/   # Container sync utilities
├── tests/                # Unit and integration tests
├── pyproject.toml        # Package configuration
├── Dockerfile            # Container image definition
└── entrypoint.sh         # Container entrypoint script
```

### Requirements

- **Python 3.13+** (uses modern Python features)
- **uv** (package manager) - https://docs.astral.sh/uv/
- **Docker** (for background mode)
- **git-bash** (on Windows)

### Windows Development

This project is designed to work on Windows using `git-bash` for proper Unix-like shell support. UTF-8 encoding is handled automatically in all shell scripts.

### Testing

```bash
# Run all tests
bash test

# Run with verbose output
bash test -v

# Run integration tests (sequential, avoid Docker conflicts)
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

### Building Docker Image

```bash
# Build image
docker build -t niteris/clud .

# Verify build
docker run --rm niteris/clud node --version
docker run --rm niteris/clud npm --version
docker run --rm niteris/clud python --version
```

## Entry Points

The package provides three CLI entry points:

- `clud` - Main CLI (router to foreground/background agents)
- `clud-bg` - Direct background agent entry point
- `clud-fb` - Feedback/fix workflow entry point

## Links

- **Awesome Claude Code**: https://github.com/hesreallyhim/awesome-claude-code
- **SuperClaude Framework**: https://github.com/SuperClaude-Org/SuperClaude_Framework
- **AB Method**: https://github.com/ayoubben18/ab-method

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
