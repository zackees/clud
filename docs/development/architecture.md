# Architecture

## Purpose

`clud` is a Python CLI that runs Claude Code in "YOLO mode" by default, eliminating permission prompts for maximum development velocity.

## Project Structure

```
src/clud/              Main package source code
├── cli.py             Main CLI entry point
├── agent_cli.py       Consolidated agent execution module
├── agent_args.py      Unified argument parser
├── agent/             Agent support subpackage
│   ├── foreground.py      Direct Claude Code execution
│   ├── foreground_args.py Argument parsing
│   └── completion.py      Completion detection
├── hooks/             Event-based hook system
│   ├── __init__.py    Core hook infrastructure
│   ├── webhook.py     Webhook handler
│   └── config.py      Hook configuration
├── daemon/            Multi-terminal daemon (Playwright)
│   ├── __init__.py        Daemon proxy class
│   ├── playwright_daemon.py  Main daemon orchestrator
│   ├── server.py          HTTP + WebSocket server
│   ├── terminal_manager.py   PTY session management
│   ├── html_template.py   xterm.js grid template
│   └── cli_handler.py     CLI handler for --daemon
├── cron/              Cron scheduler
│   ├── __init__.py        Cron proxy class
│   ├── daemon.py          Background daemon
│   ├── config.py          Configuration management
│   └── executor.py        Task execution
├── backlog/           Backlog.md parser
└── task.py            Task management system

tests/                 Unit and integration tests
├── test_*.py          Unit tests (unittest framework)
├── test_daemon.py     Daemon unit tests (33 tests)
├── integration/       E2E tests (Playwright)
│   └── test_daemon_e2e.py  Daemon E2E tests
└── artifacts/         Test output (git-ignored)

pyproject.toml         Modern Python packaging config
```

## Key Components

### CLI Router (`cli.py`)

Main entry point handling special commands and utility modes (fix, up).

**Entry points**:
- `clud` → `clud.cli:main`

**Key features**:
- Command-line argument parsing
- Mode selection (foreground, task, cron, daemon, loop)
- Claude Code detection and installation

### Foreground Agent (`agent/foreground.py`)

Direct Claude Code execution with `--dangerously-skip-permissions`.

**Responsibilities**:
- Spawning Claude Code subprocess
- YOLO mode execution (no permission prompts)
- Output streaming and handling
- Exit code propagation

### Task System (`task.py`)

File-based task execution system for running `.md` task files.

**Features**:
- Reads task instructions from markdown files
- Executes tasks via Claude Code
- Supports cron scheduling integration

### Agent Completion Detection (`agent/completion.py`)

Monitors terminal output to detect when Claude Code agent has finished.

**Capabilities**:
- Idle detection (no output for N seconds)
- Exit signal detection
- Timeout handling

### Hook System (`src/clud/hooks/`)

Event-based architecture for intercepting and forwarding execution events to external systems.

**Components**:
- `HookManager`: Singleton managing hook registration and triggering
- `HookHandler`: Protocol for implementing custom handlers
- `WebhookHandler`: HTTP webhook notifications

**Events**:
- PRE_EXECUTION
- POST_EXECUTION
- OUTPUT_CHUNK
- ERROR
- AGENT_START
- AGENT_STOP

### Multi-Terminal Daemon (`src/clud/daemon/`)

Playwright-based daemon providing 8 interactive terminals in a browser window.

**Components**:

1. **`__init__.py`** - Lazy-loading proxy class:
   - `Daemon.start()` - Launch the daemon
   - `Daemon.is_running()` - Check daemon status
   - `DaemonInfo` dataclass for status info

2. **`playwright_daemon.py`** - Main orchestrator:
   - Launches Playwright Chromium browser
   - Creates browser context and page
   - Manages daemon lifecycle
   - PID file for tracking running daemons

3. **`server.py`** - HTTP + WebSocket server:
   - `DaemonServer` class for server management
   - HTTP server serves HTML template
   - WebSocket server handles terminal I/O
   - Automatic port selection

4. **`terminal_manager.py`** - PTY session management:
   - `Terminal` class for individual terminals
   - `TerminalManager` for managing all terminals
   - Cross-platform PTY support (pywinpty/pty)
   - WebSocket forwarding for stdin/stdout

5. **`html_template.py`** - xterm.js grid template:
   - 2-column grid layout for 8 terminals
   - xterm.js CDN integration
   - VS Code Dark+ color theme
   - Auto-resize on window resize

6. **`cli_handler.py`** - CLI handler:
   - `handle_daemon_command()` entry point
   - Async runner for event loop

**Features**:
- 8 xterm.js terminals in a flex grid
- All terminals start in home directory
- Full shell access (git-bash on Windows, bash/zsh on Unix)
- WebSocket-based I/O streaming
- Clean shutdown on browser close

### Cron Scheduler (`src/clud/cron/`)

Background daemon for scheduled task execution.

**Components**:
- `CronDaemon`: Background process management
- `CronConfig`: Configuration file management
- `TaskExecutor`: Task execution with logging

**Features**:
- Cron-style scheduling syntax
- PID file for daemon tracking
- Task execution logging
- Automatic daemon restart on errors

## Package Configuration

### Modern Python Packaging

Uses setuptools with `pyproject.toml`:

```toml
[project]
name = "clud"
requires-python = ">=3.13"

[project.scripts]
clud = "clud.cli:main"
```

### Key Dependencies

- **keyring** - Secure credential storage
- **httpx** - HTTP client for API calls
- **pywinpty** - Windows terminal support
- **running-process** - Process execution utilities
- **playwright** - Browser automation for daemon
- **websockets** - WebSocket server for terminal I/O

## Code Quality Configuration

### Ruff

- 200-character line length
- Python 3.13 target
- Handles import sorting and unused import removal
- ANN ruleset for return type annotations

### Pyright

- Strict mode enabled
- `reportUnknownVariableType`: error
- `reportUnknownArgumentType`: error
- Third-party library errors given amnesty

## Windows Compatibility

The project works on Windows using git-bash:
- UTF-8 encoding handling in all shell scripts
- pywinpty dependency for Windows terminal support
- Cross-platform path handling
- Git-bash auto-detection for terminal console

## Data Flow Examples

### Foreground Agent Execution

```
User → CLI → AgentForeground → Claude Code subprocess
                ↓
         Hook System → Webhooks
                ↓
         Terminal output → User
```

### Multi-Terminal Daemon

```
User → CLI → PlaywrightDaemon → Chromium Browser
                   ↓
            DaemonServer → HTTP (HTML page)
                   ↓
            WebSocket → TerminalManager → PTY processes
                   ↓
            xterm.js ← WebSocket ← Terminal output
```

### Cron Scheduler

```
Daemon → Task due check → TaskExecutor → Claude Code
                                ↓
                          Task logs (~/.clud/logs/cron/)
```

## Testing Architecture

### Unit Tests (unittest)

- All test files use `unittest.TestCase`
- Tests have `main()` function calling `unittest.main()`
- Executed via pytest (compatible with unittest)
- Parallel execution with pytest-xdist

### E2E Tests (Playwright)

- File naming: `test_*_e2e.py`
- Unique ports per test (8950-8960 for daemon)
- Server lifecycle in async test methods
- `CI` env var to skip browser tests in CI
- Artifacts saved to `tests/integration/artifacts/`

## Related Documentation

- [Development Setup](setup.md)
- [Code Quality Standards](code-quality.md)
- [Troubleshooting](troubleshooting.md)
