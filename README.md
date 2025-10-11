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
pip install clud
```

## Quick Start

```bash
# Unleash Claude Code instantly (YOLO mode enabled by default)
clud

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
```

**Features:**
- Claude Code with dangerous permissions enabled by default
- Zero interruption workflow - no safety prompts
- Direct prompt execution for rapid iteration
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
- **Telegram notifications** for agent launch and completion

### Telegram Notifications

Get notified when your background agents launch and complete via Telegram. This feature only works with `clud bg` (background mode), not with foreground mode.

#### Quick Setup (5 Minutes)

**Step 1: Create a Telegram Bot**
1. Open Telegram and search for `@BotFather`
2. Send `/newbot` command and follow the prompts
3. Save your bot token (looks like `123456:ABC-DEF1234ghIkl...`)

**Step 2: Get Your Chat ID**
1. Search for `@userinfobot` on Telegram
2. Send `/start` command
3. Note your chat ID (a number like `123456789`)

**Step 3: Start Your Bot**
1. Search for your bot by its username
2. **IMPORTANT:** Send `/start` to your bot to initiate the conversation
   - Bots cannot send messages until you start the conversation first
   - This is a Telegram security requirement

**Step 4: Configure Credentials**

Option A - Environment Variables (Recommended):
```bash
export TELEGRAM_BOT_TOKEN="123456:ABC-DEF..."
export TELEGRAM_CHAT_ID="123456789"
clud bg --telegram
```

Option B - Project Config File:
Create a `.clud` file in your project root:
```json
{
  "telegram": {
    "enabled": true,
    "bot_token": "${TELEGRAM_BOT_TOKEN}",
    "chat_id": "${TELEGRAM_CHAT_ID}"
  }
}
```
Then run: `clud bg` (auto-detects config)

Option C - Command Line:
```bash
clud bg --telegram \
  --telegram-bot-token "123456:ABC-DEF..." \
  --telegram-chat-id "123456789"
```

**Installation:**
```bash
pip install python-telegram-bot
```

#### What You Get

- 🚀 **Launch notification** - When agent starts with container details
- ✅ **Cleanup notification** - When agent completes with duration and summary
- 📊 **Duration tracking** - See how long your tasks took
- 💬 **Bidirectional messaging** - Send messages to your agent (future feature)
- 💰 **FREE** - No API costs, completely free

#### Group Notifications

Want to notify a team? You can send notifications to Telegram groups:

1. Create a group chat in Telegram
2. **Manually add your bot to the group** (bots cannot auto-join groups)
3. Get the group chat ID:
   - Add `@userinfobot` to the group temporarily
   - It will show the group chat ID (negative number like `-987654321`)
   - Remove @userinfobot from the group
4. Use the group chat ID instead of your personal chat ID
5. Run `clud bg --telegram` with the group chat ID

Note: For groups, configure the bot's privacy settings via @BotFather using `/setprivacy` if needed.

#### Important Notes

- Telegram bots **cannot** automatically join groups - they must be manually invited
- Users **must** send `/start` to a bot before it can send personal messages
- Chat IDs for groups are negative numbers (e.g., `-123456789`)
- Chat IDs for personal chats are positive numbers (e.g., `123456789`)
- Credentials are stored via environment variables or project-local `.clud` file
- There is no global clud settings file - use env vars for cross-project bots

See [TELEGRAM_SETUP.md](TELEGRAM_SETUP.md) for detailed setup instructions and troubleshooting.

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
clud bg --telegram                # Enable Telegram notifications
```

### Telegram Notification Commands
```bash
# With credentials
clud bg --telegram --telegram-bot-token TOKEN --telegram-chat-id ID

# With environment variables (recommended)
export TELEGRAM_BOT_TOKEN="123456:ABC-DEF..."
export TELEGRAM_CHAT_ID="123456789"
clud bg --telegram
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
