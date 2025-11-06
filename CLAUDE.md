# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

### Development Setup
- `bash install` - Set up development environment with Python 3.13 virtual environment using uv
- `source activate` (or `. activate`) - Activate the virtual environment (symlinked to .venv/bin/activate or .venv/Scripts/activate on Windows)

### Testing
- `bash test` - Run unit tests (excludes E2E tests by default)
- `bash test --full` - Run full test suite including Playwright E2E tests
  - Automatically installs Playwright browsers with system dependencies
  - Tests Web UI loading and verifies no console errors
  - Takes longer than unit tests, recommended before releases
  - Test artifacts (screenshots, reports) are stored in `tests/artifacts/` (git-ignored)
- `uv run pytest tests/ -n auto -vv` - Run tests directly with pytest (parallel execution)

### Linting and Code Quality
- `bash lint` - Run Python linting with ruff and pyright
- `uv run ruff check --fix src/ tests/` - Run ruff linting with auto-fixes
- `uv run ruff format src/ tests/` - Format code using ruff
- `uv run pyright` - Type checking with pyright

### Build and Package
- `uv pip install -e ".[dev]"` - Install package in editable mode with dev dependencies
- The package builds a wheel to `dist/clud-{version}-py3-none-any.whl`

### Frontend Development
- `cd src/clud/webui/frontend && npm install` - Install frontend dependencies (SvelteKit, TypeScript, etc.)
- `cd src/clud/webui/frontend && npm run dev` - Run frontend dev server with hot reload (port 5173)
- `cd src/clud/webui/frontend && npm run build` - Build frontend for production (outputs to `build/`)
- `cd src/clud/webui/frontend && npm run preview` - Preview production build locally
- `cd src/clud/webui/frontend && npm run check` - Type-check Svelte components with TypeScript
- Frontend architecture: Svelte 5 + SvelteKit + TypeScript with `@sveltejs/adapter-static` for SPA mode

### Cleanup
- `bash clean` - Remove all build artifacts, caches, and virtual environment

### Telegram Bot Integration
- `clud --telegram` (or `clud -tg`) - Open Telegram bot landing page in browser
  - Launches a local HTTP server on an auto-assigned port
  - Opens the landing page in your default browser
  - Landing page provides:
    - Button to open the Claude Code Telegram bot (https://t.me/clud_ckl_bot)
    - Explanation of why direct iframe embedding isn't possible (Telegram security)
    - Preview of upcoming features (custom chat UI, dashboard, etc.)
  - Press Ctrl+C to stop the server
  - Note: Telegram blocks iframe embedding with X-Frame-Options for security, so the landing page provides a button to open the bot in Telegram instead

### Web UI
- `clud --webui [PORT]` (or `clud --ui [PORT]`) - Launch browser-based interface for Claude Code
  - Default port: 8888 (auto-detects if unavailable)
  - Automatically opens browser to Web UI
  - Features:
    - Real-time streaming chat interface with Claude Code
    - **Integrated terminal console** with xterm.js (split-pane layout)
    - **Backlog tab** for visualizing and managing tasks from `Backlog.md`
    - Project directory selection
    - Conversation history (stored in browser localStorage)
    - Dark/light theme toggle
    - Mobile-responsive design
    - WebSocket-based communication
    - Markdown rendering with code syntax highlighting
    - Runs in YOLO mode (no permission prompts)
  - Architecture:
    - FastAPI backend with WebSocket streaming
    - **Svelte 5 + SvelteKit + TypeScript frontend** (migrated from vanilla JS)
    - Uses `running-process` library for Claude Code execution
    - PTY-based terminal with cross-platform support
    - Static files served from `src/clud/webui/frontend/build/` (falls back to `static/` if build missing)
  - Configuration:
    - Can specify custom port: `clud --webui 3000` or `clud --ui 3000`
    - Browser auto-opens after 2-second delay
    - Server logs to console with INFO level
  - Press Ctrl+C to stop the server
  - Inspired by: [sugyan/claude-code-webui](https://github.com/sugyan/claude-code-webui)

#### Terminal Console

The Web UI includes an integrated terminal console that provides direct shell access alongside the chat interface.

**Features**:
- **Multiple Terminals**: Create multiple terminal sessions with tabbed interface
- **Split-Pane Layout**: Adjustable resize handle between chat and terminal panels
- **Full Shell Access**: Real PTY (pseudo-terminal) with ANSI color support
- **Cross-Platform**: Works on Windows (git-bash/cmd) and Unix (bash/zsh/sh)
- **Responsive Design**: Stacks vertically on mobile devices

**Usage**:
- **New Terminal**: Click the "+" button in the terminal tabs area
- **Switch Terminals**: Click on tab to switch between terminals
- **Close Terminal**: Click "×" on the tab to close a terminal
- **Clear Terminal**: Click the trash icon (🗑️) to clear the active terminal
- **Toggle Panel**: Click the arrow icon (⬇️) to collapse/expand terminal panel
- **Resize Panels**: Drag the vertical resize handle between chat and terminal

**Keyboard Shortcuts**:
- All standard terminal shortcuts work (Ctrl+C, Ctrl+D, Ctrl+Z, etc.)
- Tab completion, command history (↑/↓), and line editing work as expected
- Copy/paste: Use browser's standard shortcuts (Ctrl+C/V or Cmd+C/V)

**Shell Behavior**:
- **Windows**: Automatically uses git-bash if available, falls back to cmd.exe
- **Unix/Linux**: Uses user's default shell ($SHELL) or /bin/bash
- **Working Directory**: Terminals start in the selected project directory
- **Environment**: Inherits environment variables from the Web UI server

**Architecture**:
- **Frontend**: xterm.js terminal emulator with FitAddon for responsive sizing
- **Backend**: PTY manager (`pty_manager.py`) with platform-specific implementations
  - Unix: Native `pty.fork()` with file descriptor I/O
  - Windows: `pywinpty` library wrapping Windows ConPTY
- **Communication**: WebSocket endpoint (`/ws/term`) for real-time I/O streaming
- **Terminal Handler**: `terminal_handler.py` bridges WebSocket and PTY with async I/O

**Components**:
- `src/clud/webui/pty_manager.py` - Cross-platform PTY session management
- `src/clud/webui/terminal_handler.py` - WebSocket handler for terminal I/O
- `src/clud/webui/frontend/src/lib/components/Terminal.svelte` - Terminal component (Svelte)
- `tests/test_pty_manager.py` - PTY manager unit tests
- `tests/test_terminal_handler.py` - Terminal handler unit tests

**Security Considerations**:
- **Localhost Only**: Terminal provides full shell access - only run on trusted localhost
- **No Authentication**: Current implementation has no authentication mechanism
- **Network Deployment**: Requires authentication, resource limits, and security hardening
- **Working Directory Validation**: Terminal starts in validated project directory
- **Environment Inheritance**: Shell inherits all environment variables from server

**Troubleshooting**:
- **Terminal Not Appearing**: Check browser console for WebSocket connection errors
- **Commands Not Working**: Verify shell is running (check for shell prompt)
- **Garbled Output**: Ensure terminal is properly sized (resize window to trigger refit)
- **Windows Issues**: Ensure git-bash is installed at `C:\Program Files\Git\bin\bash.exe`
- **Connection Lost**: Terminal will show "[Connection closed]" - create a new terminal tab

#### Backlog Tab

The Web UI includes an integrated Backlog tab for visualizing and managing project tasks from a `Backlog.md` file in your project directory.

**Features**:
- **Task Visualization**: View tasks organized by status (To Do, In Progress, Done)
- **Status Filtering**: Filter tasks by status using button controls
- **Search**: Search tasks by title or description
- **Real-time Updates**: Refresh button to reload tasks from `Backlog.md`
- **Task Statistics**: Header displays task counts by status
- **Markdown-based**: Tasks are stored in a simple `Backlog.md` file in your project root

**Usage**:
1. **Create Backlog.md**: Create a `Backlog.md` file in your project root directory
2. **Open Web UI**: Launch the Web UI with `clud --webui`
3. **Navigate to Backlog Tab**: Click the "Backlog" tab in the navigation bar
4. **View Tasks**: Tasks are automatically loaded and displayed by status
5. **Filter Tasks**: Click status buttons (All, To Do, In Progress, Done) to filter
6. **Search Tasks**: Use the search input to find tasks by title or description
7. **Refresh**: Click the refresh button to reload tasks from the file

**Backlog.md Format**:

The parser supports GitHub-style task lists with status sections and optional metadata:

```markdown
# Backlog

## To Do
- [ ] #1 Add user authentication (priority: high)
  - Implement OAuth2 flow
  - Add JWT token handling
- [ ] #2 Create dashboard UI (priority: medium)
  - Design wireframes
  - Implement frontend components

## In Progress
- [ ] #3 Fix login bug
  - Debug session handling
  - Add error logging

## Done
- [x] #4 Setup project structure
  - Initialize repository
  - Configure build system
- [x] #5 Write documentation (priority: low)
  - README.md completed
  - API docs added
```

**Task Format Details**:
- **Task ID**: `#N` format (e.g., `#1`, `#2`) - auto-extracted from task text
- **Status**: Determined by section heading (To Do, In Progress, Done)
- **Checkbox**: `- [ ]` for incomplete, `- [x]` for complete
- **Priority**: Optional inline metadata `(priority: high|medium|low)`
- **Description**: Indented sub-items under the main task
- **Timestamps**: Automatically tracked (created_at, updated_at)

**API Endpoint**:
- **Endpoint**: `GET /api/backlog`
- **Response Format**:
  ```json
  {
    "tasks": [
      {
        "id": "1",
        "title": "Add user authentication",
        "status": "todo",
        "description": "Implement OAuth2 flow and JWT tokens",
        "priority": "high",
        "created_at": 1736899200,
        "updated_at": 1736899200
      }
    ]
  }
  ```
- **Error Handling**: Returns empty tasks array if `Backlog.md` is missing or unreadable

**Architecture**:
- **Backend**: Backlog parser (`backlog/parser.py`) reads and parses `Backlog.md`
- **API Handler**: `BacklogHandler` in `webui/api.py` provides REST endpoint
- **Frontend**: Svelte component (`frontend/src/lib/components/Backlog.svelte`)
- **Data Flow**: File → Parser → API → WebSocket → UI

**Components**:
- `src/clud/backlog/parser.py` - Markdown parser for Backlog.md (includes BacklogTask, StatusType, PriorityType models)
- `src/clud/webui/api.py` - BacklogHandler for API endpoint
- `src/clud/webui/frontend/src/lib/components/Backlog.svelte` - Backlog UI component
- `tests/test_backlog_parser.py` - Parser unit tests (15 tests)
- `tests/test_backlog_tab_e2e.py` - E2E tests (9 tests)

**Testing**:
- **Unit Tests**: `uv run pytest tests/test_backlog_parser.py -vv` (15 tests)
- **E2E Tests**: `uv run pytest tests/test_backlog_tab_e2e.py -vv` (9 tests)
- **Full Suite**: `bash test --full` includes all Backlog tests

**Limitations**:
- **Read-Only**: Current implementation only reads tasks (no editing via UI)
- **Single File**: Only supports `Backlog.md` in project root (no custom paths)
- **No Persistence**: Task updates must be made by editing `Backlog.md` directly
- **No Sorting**: Tasks displayed in file order (no custom sorting)

**Future Enhancements**:
- AI agent integration for automated task management
- Task editing and creation via UI
- Task sorting and grouping options
- Multiple backlog file support
- Task dependencies and relationships

### Hook System and Message Handler API

The hook system provides an event-based architecture for intercepting and forwarding execution events to external systems (Telegram, webhooks, etc.).

**Hook System** (`src/clud/hooks/`):
- **Events**: PRE_EXECUTION, POST_EXECUTION, OUTPUT_CHUNK, ERROR, AGENT_START, AGENT_STOP
- **HookManager**: Singleton that manages hook registration and event triggering
- **HookHandler Protocol**: Interface for implementing custom hook handlers
- **TelegramHookHandler**: Built-in handler for streaming output to Telegram
- **WebhookHandler**: Built-in handler for HTTP webhook notifications

**Message Handler API** (`src/clud/api/`):
- **Purpose**: Unified API for routing messages from multiple client types to clud instances
- **MessageHandler**: Core routing logic with session management
- **InstancePool**: Manages lifecycle of clud subprocess instances
  - Automatic instance reuse per session
  - Idle timeout and cleanup (default: 30 minutes)
  - Max instances limit (default: 100)
- **FastAPI Server**: REST and WebSocket endpoints
  - `POST /api/message` - Send message to clud instance
  - `GET /api/instances` - List all active instances
  - `DELETE /api/instances/{id}` - Delete an instance
  - `WebSocket /ws/{instance_id}` - Real-time output streaming

**Testing**:
- `tests/test_hooks.py` - Hook system unit tests
- `tests/test_api_models.py` - API models unit tests
- `tests/test_message_handler.py` - Message handler unit tests
- `tests/test_instance_manager.py` - Instance manager unit tests
- `tests/test_webui_e2e.py` - End-to-end Playwright tests for Web UI (run with `bash test --full`)

### Telegram Bot API Abstraction

The Telegram integration uses an abstract API interface that allows for testing without real Telegram bot tokens or network calls.

**API Implementations** (`src/clud/telegram/`):
- **TelegramBotAPI** (`api_interface.py`): Abstract base class defining the interface
  - Provides type-safe abstractions for Telegram operations
  - No direct dependency on `python-telegram-bot` library types
  - All methods are async and fully typed
- **RealTelegramBotAPI** (`api_real.py`): Production implementation
  - Wraps `python-telegram-bot` library
  - Converts between abstract types and telegram library types
  - Handles real Telegram API calls
- **FakeTelegramBotAPI** (`api_fake.py`): In-memory testing implementation
  - Simulates Telegram bot behavior without network calls
  - Stores messages in memory for inspection
  - Configurable latency and error injection
  - Deterministic behavior for reliable tests
- **MockTelegramBotAPI** (`tests/mocks/telegram_api.py`): Mock utilities
  - Based on `unittest.mock.AsyncMock`
  - Helper functions for common assertions
  - Pre-configured mock builders

**Configuration** (`api_config.py`, `config.py`):
- **TelegramAPIConfig**: Configuration for API implementation mode
  - `implementation`: "real", "fake", or "mock"
  - `bot_token`: Telegram bot token (required for "real" mode)
  - `fake_delay_ms`: Delay in ms for fake mode (default: 100)
  - `fake_error_rate`: Error rate 0.0-1.0 for fake mode (default: 0.0)
- **TelegramIntegrationConfig**: Main configuration with `api` field
  - Integrates API config with telegram, web, sessions, and logging config
  - Supports loading from environment variables, files, or defaults

**Factory** (`api_factory.py`):
- **create_telegram_api()**: Creates appropriate implementation based on config
  - Auto-detects mode from environment variables
  - Defaults to "fake" when no token provided
  - Defaults to "real" when token provided
  - Supports explicit override via `TELEGRAM_API_MODE`

**Environment Variables**:
```bash
# Telegram API Mode Selection
export TELEGRAM_API_MODE=fake           # "real" | "fake" | "mock"
export TELEGRAM_BOT_TOKEN=<token>       # Required for "real" mode
export TELEGRAM_FAKE_DELAY=100          # Delay in ms for fake mode (default: 100)
export TELEGRAM_FAKE_ERROR_RATE=0.0     # Error rate 0.0-1.0 for fake mode (default: 0.0)
```

**Testing with Fake API**:
```python
from clud.telegram.api_config import TelegramAPIConfig
from clud.telegram.api_factory import create_telegram_api

# Create fake API for testing (zero delay, no errors)
config = TelegramAPIConfig.for_testing(implementation="fake")
api = create_telegram_api(config=config)

# Send messages and inspect results
await api.send_message(chat_id="12345", text="Hello!")
messages = api.get_sent_messages("12345")
assert len(messages) == 1
assert messages[0].text == "Hello!"
```

**Test Coverage**:
- `tests/test_telegram_api_interface.py` - Interface and config tests
- `tests/test_telegram_api_fake.py` - Fake implementation tests (17 tests)
- `tests/test_telegram_bot_handler_integration.py` - Bot handler with fake API (12 tests)
- `tests/test_telegram_hook_handler_integration.py` - Hook handler with fake API (9 tests)
- `tests/test_telegram_messenger_integration.py` - Messenger with fake API (6 tests)
- `tests/test_telegram_e2e.py` - End-to-end integration tests (15 tests)

**Benefits**:
- ✅ Test telegram functionality without network calls or bot tokens
- ✅ Deterministic test behavior (no flaky tests)
- ✅ Fast test execution (no real API latency)
- ✅ Type-safe abstractions (no third-party library type issues)
- ✅ Easy to swap implementations (real ↔ fake ↔ mock)
- ✅ Comprehensive test coverage across all telegram components

## Troubleshooting

### Claude Code Installation Issues

The `clud` tool automatically installs Claude Code when it's not detected on your system. However, the official npm package `@anthropic-ai/claude-code` may occasionally have issues.

#### Automatic Fallback Strategies

When installation fails, `clud` automatically tries multiple methods:

1. **Local --prefix install** (default, recommended)
   - Installs `@latest` to `~/.clud/npm` directory using `npm install --prefix`
   - Isolated from global npm installations
   - Controlled by `clud`

2. **Global install with isolated prefix** (automatic fallback)
   - Falls back if local install fails with module errors
   - Uses `npm install -g` with `NPM_CONFIG_PREFIX=~/.clud/npm` environment variable
   - Installs to `~/.clud/npm` (same as default method, not system-wide)
   - Works with both bundled nodejs-wheel npm and system npm

3. **Specific version install** (automatic fallback)
   - Falls back if global install also fails
   - Tries known-working version (e.g., `v0.6.0`)
   - Installs to `~/.clud/npm` using `npm install --prefix`
   - May use older but more stable version

**Technical Detail**: `clud` bundles its own npm via `nodejs-wheel`. To prevent npm global installs from going to the virtual environment (where they'd be inaccessible), we set the `NPM_CONFIG_PREFIX` environment variable to `~/.clud/npm` for all npm operations. This ensures all installation methods (--prefix, -g, or specific version) install to the same controlled location.

#### Common Installation Errors

**"Cannot find module '../lib/cli.js'"**
- **Cause**: Broken npm package structure (missing internal files)
- **Solution**: `clud` automatically tries global install and specific version fallbacks
- **Manual workaround**: Install globally with `npm install -g @anthropic-ai/claude-code@latest`

**"EACCES" or "permission denied"**
- **Cause**: Insufficient permissions for npm installation
- **Solution**: Fix npm permissions following [npm docs](https://docs.npmjs.com/resolving-eacces-permissions-errors)
- **Alternative**: Use `sudo` for global install (not recommended on shared systems)

**"ENOTFOUND" or network errors**
- **Cause**: Network connectivity issues or npm registry unavailable
- **Solution**: Check internet connection, try again later
- **Alternative**: Install behind proxy with appropriate npm configuration

**Installation succeeded but executable not found**
- **Cause**: npm installed to unexpected location
- **Solution**: Check `~/.clud/npm/node_modules/.bin/` for `claude` or `claude.cmd`
- **Manual workaround**: Set `PATH` to include the npm bin directory

#### Manual Installation Methods

If automatic installation fails completely:

1. **Global npm install**:
   ```bash
   npm install -g @anthropic-ai/claude-code@latest
   ```

2. **Direct download from Anthropic**:
   - Visit: https://claude.ai/download
   - Download installer for your platform
   - Follow installation instructions

3. **Clear npm cache and retry**:
   ```bash
   npm cache clean --force
   clud --install-claude
   ```

4. **Use clud installation command**:
   ```bash
   clud --install-claude
   ```

#### Verifying Installation

Once installed (automatically or manually), verify with:

```bash
claude --version
```

The `clud` tool will automatically detect Claude Code in:
- `~/.clud/npm/` (local installation)
- System PATH (global npm installation)
- Common Windows npm locations (`%APPDATA%\npm\`)

#### Getting Help

If installation issues persist:
- Check clud logs for detailed error messages
- Review the troubleshooting guidance printed by failed installations
- Report issues at: https://github.com/anthropics/claude-code/issues
- Note: Installation errors from the official npm package are Anthropic's responsibility

## Architecture

### Purpose
`clud` is a Python CLI that runs Claude Code in "YOLO mode" by default, eliminating permission prompts for maximum development velocity.

### Project Structure
- `src/clud/` - Main package source code
- `src/clud/cli.py` - Main CLI entry point
- `src/clud/agent_foreground.py` - Handles Claude Code execution in YOLO mode
- `src/clud/task.py` - File-based task execution system
- `tests/` - Unit and integration tests using pytest
  - `tests/artifacts/` - Test output artifacts (screenshots, reports) - git-ignored
- `pyproject.toml` - Modern Python packaging configuration

### Key Components
- **CLI Router** (`cli.py`): Main entry point handling special commands and utility modes (fix, up)
- **Foreground Agent** (`agent_foreground.py`): Direct Claude Code execution with `--dangerously-skip-permissions`
- **Task System** (`task.py`): File-based task execution system
- **Agent Completion Detection** (`agent_completion.py`): Monitors terminal for idle detection
- **Hook System** (`src/clud/hooks/`): Event-based architecture for intercepting and forwarding execution events
  - `hooks/__init__.py`: Core hook infrastructure (HookManager, HookEvent, HookContext, HookHandler)
  - `hooks/telegram.py`: Telegram-specific hook handler for real-time output streaming
  - `hooks/webhook.py`: Generic webhook handler for HTTP-based integrations
  - `hooks/config.py`: Configuration loading and validation
- **Message Handler API** (`src/clud/api/`): Unified API for routing messages to clud instances
  - `api/models.py`: Data models (MessageRequest, MessageResponse, InstanceInfo, ExecutionResult)
  - `api/message_handler.py`: Core message routing logic with session management
  - `api/instance_manager.py`: Subprocess lifecycle management (CludInstance, InstancePool)
  - `api/server.py`: FastAPI server with REST and WebSocket endpoints
- **Web UI** (`src/clud/webui/`): Browser-based interface for Claude Code
  - `webui/server.py`: FastAPI application with WebSocket support
  - `webui/api.py`: Handler classes for chat, projects, and history
  - `webui/pty_manager.py`: Cross-platform PTY session management for terminals
  - `webui/terminal_handler.py`: WebSocket handler for terminal I/O streaming
  - `webui/frontend/`: Svelte 5 + SvelteKit + TypeScript frontend (replaces `webui/static/`)
    - `frontend/src/lib/components/`: UI components (Chat, Terminal, DiffViewer, Settings, History)
    - `frontend/src/lib/stores/`: Svelte stores for state management (app, chat, settings)
    - `frontend/src/lib/services/`: WebSocket and API services
    - `frontend/build/`: Production build output (served by FastAPI)

### Package Configuration
- Uses setuptools with pyproject.toml for modern Python packaging
- Entry point:
  - `clud` → `clud.cli:main`
- Supports Python 3.13+
- Key dependencies: keyring, httpx, pywinpty (Windows), running-process

### Development Tools
- **uv** - Fast Python package installer and virtual environment manager
- **ruff** - Fast Python linter and formatter (configured for 200 char line length)
- **pyright** - Type checker with strict mode
- **pytest** - Testing framework with xdist for parallel execution
- **Playwright** - Browser automation for end-to-end testing (Chromium headless)

### Code Quality Configuration
- Ruff configured with 200-character line length and Python 3.13 target
- Ruff handles import sorting and unused import removal automatically
- Pyright configured for strict type checking with Python 3.13

### Windows Compatibility
- The project is designed to work on Windows using git-bash
- UTF-8 encoding handling in all shell scripts
- pywinpty dependency for Windows terminal support

## Code Quality Standards

### Linting Requirement
- **MANDATORY**: After ANY code editing (creating, modifying, or deleting Python files), you MUST run `bash lint`
- This ensures all code changes pass ruff linting and pyright type checking before considering the task complete
- The lint check must pass successfully - address all errors and warnings before marking work as done
- This applies to all Python code in `src/` and `tests/` directories

### Exception Handling
- **NEVER** use bare `except Exception: pass` or similar patterns that silently ignore exceptions
- All caught exceptions MUST be logged at minimum with appropriate context
- Use specific exception types when possible rather than catching broad `Exception`
- If an exception truly needs to be suppressed, use `contextlib.suppress()` and document why
- **CRITICAL: KeyboardInterrupt Handling**
  - **NEVER** silently catch or suppress `KeyboardInterrupt` exceptions
  - **RECOMMENDED**: Use the `handle_keyboard_interrupt()` utility from `clud.util` for centralized handling:
    ```python
    from clud.util import handle_keyboard_interrupt

    # Simple usage
    result = handle_keyboard_interrupt(risky_operation, arg1, arg2)

    # With cleanup and logging
    result = handle_keyboard_interrupt(
        risky_operation,
        arg1,
        arg2,
        cleanup=cleanup_function,
        logger=logger,
        log_message="Operation interrupted by user"
    )
    ```
  - **Manual handling** when `handle_keyboard_interrupt()` isn't suitable:
    ```python
    try:
        operation()
    except KeyboardInterrupt:
        raise  # MANDATORY: Always re-raise KeyboardInterrupt
    except Exception as e:
        logger.error(f"Operation failed: {e}")
    ```
  - KeyboardInterrupt (Ctrl+C) is a user signal to stop execution - suppressing it creates unresponsive processes
  - This applies to ALL exception handlers, including hook handlers, cleanup code, and background tasks
  - The `handle_keyboard_interrupt()` utility:
    - Ensures KeyboardInterrupt is ALWAYS re-raised
    - Optionally calls cleanup function before re-raising
    - Handles cleanup failures gracefully (logs but doesn't suppress interrupt)
    - Optionally logs the interrupt with custom message
    - See `src/clud/util.py` and `tests/test_util.py` for implementation and examples
  - **Linter Support**:
    - Ruff's **BLE001** (blind-except) rule can detect overly broad exception handlers that catch `KeyboardInterrupt`
    - BLE001 is NOT active by default - must be explicitly enabled with `--select BLE001` or in pyproject.toml
    - Consider enabling BLE001 in the future for automatic detection of this pattern
    - Currently relying on manual code review for KeyboardInterrupt handling
- Example of proper exception handling:
  ```python
  try:
      risky_operation()
  except SpecificException as e:
      logger.warning(f"Operation failed with expected error: {e}")
      # Handle or re-raise as appropriate
  ```

### Python Path Management
- **NEVER** use `sys.path.insert()` or any other `sys.path` manipulation
- Path problems are typically caused by trying to directly execute package code instead of using proper tools
- **ALWAYS** use `uv run` for running Python scripts that need access to package dependencies
- If you encounter import errors, the solution is to use `uv run`, not to modify `sys.path`
- `sys.path` imports before regular imports are strictly forbidden and should be flagged as code quality violations

### Type Annotations
- **Return Type Annotations**: Enforced via ruff's ANN ruleset (flake8-annotations)
  - **MANDATORY**: All functions must have explicit return type annotations (e.g., `-> None`, `-> str`, `-> int`)
  - This includes public functions (ANN201), private functions (ANN202), and special methods like `__init__` (ANN204)
  - Function arguments must also have type annotations (ANN001)
  - `typing.Any` is allowed when necessary (ANN401 is ignored)
- **Type Checking**: Strict type checking is enforced via pyright
  - Use specific types rather than `Any` when possible
  - `reportUnknownVariableType` and `reportUnknownArgumentType` are configured as **errors**
- **Third-Party Library Amnesty**: Errors from third-party libraries (keyring, telegram, etc.) should be given lint amnesty
  - These errors from external dependencies are acceptable and should NOT be "fixed" with type ignore comments
  - Always fix type errors in code you control (src/clud/ and tests/)
  - Common acceptable errors from third-party: `reportUnknownVariableType`, `reportUnknownArgumentType` from keyring, telegram, etc.
  - Do NOT add `# type: ignore` or `# pyright: ignore` comments for third-party library type issues
  - The goal: zero unknown types in our code, but accept incomplete type stubs from dependencies

### Test Framework Standard
- **MANDATORY**: All unit tests MUST use the `unittest` framework
- All test files MUST have a `main` function that runs `unittest.main()`
- Tests are executed via pytest (which is compatible with unittest), but the test code itself must use unittest
- Example test file structure:
  ```python
  import unittest

  class TestMyFeature(unittest.TestCase):
      def test_something(self) -> None:
          self.assertEqual(1, 1)

  if __name__ == "__main__":
      unittest.main()
  ```
- This allows tests to be run both via pytest and directly as Python scripts

### Process Execution Standard
- **MANDATORY**: Prefer `running-process` over `subprocess` for executing external commands
- **CRITICAL**: NEVER use `subprocess.run()` with `capture_output=True` for long-running processes
  - `capture_output=True` buffers stdout/stderr in memory, causing processes to **stall** when buffers fill
  - This is especially problematic for commands like `lint-test`, `pytest`, or any process with substantial output
- **ALWAYS** use `RunningProcess.run_streaming()` for commands that may produce significant output
  - Streams output to console in real-time without buffering
  - Prevents stdout/stderr buffer stalls
  - Provides better user experience with live output
- Example of proper process execution:
  ```python
  from running_process import RunningProcess

  # Good: Streaming output for long-running processes
  returncode = RunningProcess.run_streaming(["lint-test"])

  # Bad: Can stall on long output!
  # result = subprocess.run(["lint-test"], capture_output=True)
  ```
- **When to use subprocess**: Only for simple, short-lived commands where you need to capture a small amount of output
- **Why this matters**: Stdout/stderr buffers have limited capacity (typically 64KB). When full, the process blocks until the buffer is read. With `capture_output=True`, the buffer is only read after the process completes, creating a deadlock for processes with large output.

### Playwright E2E Testing Protocol
- **File naming**: `tests/test_*_e2e.py` (excluded from pyright type checking)
- **Run command**: `bash test --full` (auto-installs Playwright browsers)
- **Unique ports**: Each E2E test must use a unique port (e.g., 8899, 8902, 8903) to avoid conflicts
- **Server lifecycle**: Start in `setUpClass()`, stop in `tearDownClass()`, use `CLUD_NO_BROWSER=1` env var
- **Console error filtering**: Ignore "WebSocket" and "favicon" errors, fail on all others
- **Test artifacts**: Save screenshots/reports to `tests/artifacts/` (git-ignored)
- **Standard template**:
  ```python
  import unittest
  from pathlib import Path
  import subprocess, time, os
  from playwright.sync_api import ConsoleMessage, sync_playwright

  class TestFeatureE2E(unittest.TestCase):
      server_process: subprocess.Popen[bytes] | None = None
      server_url: str = "http://localhost:PORT"  # Use unique port
      startup_timeout: int = 30

      @classmethod
      def setUpClass(cls) -> None:
          env = os.environ.copy()
          env["CLUD_NO_BROWSER"] = "1"
          cls.server_process = subprocess.Popen(
              ["uv", "run", "--no-sync", "clud", "--webui", "PORT"],
              env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
              cwd=str(Path(__file__).parent.parent)
          )
          # Poll /health endpoint until ready (see test_webui_e2e.py for full example)

      @classmethod
      def tearDownClass(cls) -> None:
          if cls.server_process:
              cls.server_process.terminate()
              try:
                  cls.server_process.wait(timeout=5)
              except subprocess.TimeoutExpired:
                  cls.server_process.kill()

      def test_feature(self) -> None:
          console_errors: list[str] = []
          def on_console_message(msg: ConsoleMessage) -> None:
              if msg.type == "error" and "WebSocket" not in msg.text and "favicon" not in msg.text:
                  console_errors.append(msg.text)

          with sync_playwright() as playwright:
              browser = playwright.chromium.launch(headless=True)
              page = browser.new_page()
              page.on("console", on_console_message)
              page.goto(self.server_url, wait_until="networkidle", timeout=30000)
              # Test assertions here
              self.assertEqual(len(console_errors), 0)
              browser.close()
  ```
