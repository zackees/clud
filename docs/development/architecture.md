# Architecture

## Purpose

`clud` is a Python CLI that runs Claude Code in "YOLO mode" by default, eliminating permission prompts for maximum development velocity.

## Project Structure

```
src/clud/              Main package source code
├── cli.py             Main CLI entry point
├── agent_foreground.py    Claude Code execution in YOLO mode
├── task.py            File-based task execution system
├── agent_completion.py    Terminal idle detection
├── hooks/             Event-based hook system
│   ├── __init__.py    Core hook infrastructure
│   ├── telegram.py    Telegram hook handler
│   ├── webhook.py     Webhook handler
│   └── config.py      Hook configuration
├── api/               Message handler API
│   ├── models.py      Data models
│   ├── message_handler.py    Message routing logic
│   ├── instance_manager.py   Subprocess lifecycle
│   └── server.py      FastAPI server
├── webui/             Browser-based interface
│   ├── server.py      FastAPI application
│   ├── api.py         API handlers
│   ├── pty_manager.py PTY session management
│   ├── terminal_handler.py   Terminal I/O
│   └── frontend/      Svelte 5 + SvelteKit UI
├── telegram/          Telegram bot integration
└── backlog/           Backlog.md parser

tests/                 Unit and integration tests
├── test_*.py          Unit tests (unittest framework)
├── test_*_e2e.py      E2E tests (Playwright)
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
- Mode selection (foreground, task, cron, webui, telegram)
- Claude Code detection and installation

### Foreground Agent (`agent_foreground.py`)

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

### Agent Completion Detection (`agent_completion.py`)

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
- `TelegramHookHandler`: Streams output to Telegram
- `WebhookHandler`: HTTP webhook notifications

**Events**:
- PRE_EXECUTION
- POST_EXECUTION
- OUTPUT_CHUNK
- ERROR
- AGENT_START
- AGENT_STOP

### Message Handler API (`src/clud/api/`)

Unified API for routing messages from multiple client types to clud instances.

**Components**:
- `MessageHandler`: Core routing logic with session management
- `InstancePool`: Subprocess lifecycle management
  - Automatic instance reuse per session
  - Idle timeout and cleanup (default: 30 minutes)
  - Max instances limit (default: 100)
- `FastAPI Server`: REST and WebSocket endpoints
  - `POST /api/message` - Send message to clud instance
  - `GET /api/instances` - List active instances
  - `DELETE /api/instances/{id}` - Delete instance
  - `WebSocket /ws/{instance_id}` - Real-time streaming

### Web UI (`src/clud/webui/`)

Browser-based interface for Claude Code with real-time streaming.

**Backend**:
- FastAPI application with WebSocket support
- Handler classes for chat, projects, and history
- Cross-platform PTY session management
- Terminal I/O streaming

**Frontend** (Svelte 5 + SvelteKit + TypeScript):
- `frontend/src/lib/components/`: UI components (Chat, Terminal, DiffViewer, Settings, History)
- `frontend/src/lib/stores/`: Svelte stores for state management
- `frontend/src/lib/services/`: WebSocket and API services
- `frontend/build/`: Production build output (served by FastAPI)

**Static files served from**:
- `src/clud/webui/frontend/build/` (production)
- Falls back to `static/` if build missing

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
- **fastapi** - Web framework for APIs
- **uvicorn** - ASGI server
- **python-telegram-bot** - Telegram bot integration

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
         Hook System → Telegram/Webhooks
                ↓
         Terminal output → User
```

### Web UI Chat

```
Browser → WebSocket → ChatHandler → CludInstance (subprocess)
                                         ↓
                                    Claude Code
                                         ↓
Browser ← WebSocket ← Output streaming ←
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
- Unique ports per test (8899, 8902, 8903, etc.)
- Server lifecycle in `setUpClass()`/`tearDownClass()`
- `CLUD_NO_BROWSER=1` env var to prevent browser launch
- Console error filtering (ignore WebSocket/favicon errors)
- Artifacts saved to `tests/artifacts/`

## Related Documentation

- [Development Setup](setup.md)
- [Code Quality Standards](code-quality.md)
- [Troubleshooting](troubleshooting.md)
