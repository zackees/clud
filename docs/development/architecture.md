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
│   ├── server.py      FastAPI application (174 lines) - Main orchestration
│   ├── server_config.py   Configuration & port management (134 lines)
│   ├── static_routes.py   Static file serving routes (98 lines)
│   ├── websocket_routes.py   WebSocket endpoints (129 lines)
│   ├── rest_routes.py   REST API endpoints (483 lines)
│   ├── api.py         API handlers
│   ├── pty_manager.py PTY session management
│   ├── terminal_handler.py   Terminal I/O
│   └── frontend/      Svelte 5 + SvelteKit UI
├── cluster/           Cluster management server
│   ├── app.py         FastAPI application (454 lines) - Main orchestration
│   ├── auth_dependencies.py   Authentication dependencies (59 lines)
│   └── routes/        API route modules
│       ├── __init__.py    Route imports
│       ├── agents.py  Agent management routes (215 lines)
│       └── daemons.py Daemon management routes (92 lines)
├── service/           Service management server
│   ├── server.py      Main server (455 lines) - Core orchestration
│   ├── telegram_manager.py   Telegram bot management (155 lines)
│   └── handlers/      Request handlers
│       ├── __init__.py    Handler exports
│       ├── agent_routes.py   Agent operations (195 lines)
│       └── daemon_routes.py  Daemon operations (140 lines)
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

### Cluster Management Server (`src/clud/cluster/`)

Multi-tenant agent and daemon management server. **Refactored to 454 lines** (38.9% reduction from 743 lines).

**Modular Architecture**:

1. **`app.py` (454 lines)** - Main orchestration:
   - FastAPI application setup
   - CORS and middleware configuration
   - Route registration
   - WebSocket management
   - Telegram bot integration
   - Error handling

2. **`auth_dependencies.py` (59 lines)** - Authentication:
   - Token validation dependencies
   - Security utilities
   - Authentication helpers

3. **`routes/agents.py` (215 lines)** - Agent management:
   - `POST /agents` - Create agent instance
   - `GET /agents` - List agents
   - `GET /agents/{agent_id}` - Get agent details
   - `POST /agents/{agent_id}/message` - Send message
   - `DELETE /agents/{agent_id}` - Delete agent
   - `POST /agents/{agent_id}/kill` - Force kill agent
   - WebSocket streaming support

4. **`routes/daemons.py` (92 lines)** - Daemon management:
   - `POST /daemons` - Create daemon
   - `GET /daemons` - List daemons
   - `GET /daemons/{daemon_id}` - Get daemon details
   - `DELETE /daemons/{daemon_id}` - Delete daemon

**Key Features**:
- Multi-user authentication with token-based auth
- Real-time WebSocket streaming for agent output
- Telegram bot integration per agent
- Automatic cleanup and lifecycle management
- Process isolation and security

### Service Management Server (`src/clud/service/`)

Local service for managing agent and daemon processes. **Refactored to 455 lines** (34.6% reduction from 696 lines).

**Modular Architecture**:

1. **`server.py` (455 lines)** - Core orchestration:
   - FastAPI application setup
   - Process lifecycle management
   - Daemon coordination
   - Event handling
   - Server startup and shutdown

2. **`telegram_manager.py` (155 lines)** - Telegram integration:
   - Bot lifecycle management
   - Message routing to agents
   - Telegram webhook handling
   - Bot registration and cleanup

3. **`handlers/agent_routes.py` (195 lines)** - Agent operations:
   - `POST /agents` - Create agent
   - `GET /agents` - List agents
   - `GET /agents/{agent_id}` - Get agent info
   - `POST /agents/{agent_id}/send` - Send message
   - `DELETE /agents/{agent_id}` - Stop agent
   - `POST /agents/{agent_id}/kill` - Force kill

4. **`handlers/daemon_routes.py` (140 lines)** - Daemon operations:
   - `POST /daemons/start` - Start daemon
   - `POST /daemons/stop` - Stop daemon
   - `GET /daemons/status` - Get daemon status
   - `GET /daemons/logs` - Get daemon logs
   - Daemon lifecycle coordination

**Key Features**:
- Local process management without authentication
- Integrated Telegram bot support
- Daemon background execution
- Process monitoring and cleanup
- Log management

### Web UI (`src/clud/webui/`)

Browser-based interface for Claude Code with real-time streaming. **Refactored to 174 lines** (80.4% reduction from 886 lines).

**Modular Architecture**:

1. **`server.py` (174 lines)** - Main orchestration:
   - FastAPI application initialization
   - Route registration and module integration
   - Lifespan management
   - Server startup and shutdown coordination

2. **`server_config.py` (134 lines)** - Configuration management:
   - Port selection and validation
   - Frontend path resolution (build vs. static fallback)
   - Frontend URL generation
   - Configuration utilities

3. **`static_routes.py` (98 lines)** - Static file serving:
   - Frontend build serving
   - SPA routing with index.html fallback
   - Static asset handling
   - MIME type configuration

4. **`websocket_routes.py` (129 lines)** - WebSocket endpoints:
   - Real-time chat streaming
   - Terminal session WebSockets
   - Connection lifecycle management
   - Error handling

5. **`rest_routes.py` (483 lines)** - REST API endpoints:
   - Chat operations (send, history, list)
   - Project management
   - History management
   - Terminal operations
   - Settings endpoints
   - Health checks

**Supporting Modules**:
- `api.py` - Handler classes for chat, projects, and history
- `pty_manager.py` - Cross-platform PTY session management
- `terminal_handler.py` - Terminal I/O streaming

**Frontend** (Svelte 5 + SvelteKit + TypeScript):
- `frontend/src/lib/components/`: UI components (Chat, Terminal, DiffViewer, Settings, History)
- `frontend/src/lib/stores/`: Svelte stores for state management
- `frontend/src/lib/services/`: WebSocket and API services
- `frontend/build/`: Production build output (served by FastAPI)

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
