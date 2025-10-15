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
- `clud --webui [PORT]` - Launch browser-based interface for Claude Code
  - Default port: 8888 (auto-detects if unavailable)
  - Automatically opens browser to Web UI
  - Features:
    - Real-time streaming chat interface with Claude Code
    - **Integrated terminal console** with xterm.js (split-pane layout)
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
    - Can specify custom port: `clud --webui 3000`
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
