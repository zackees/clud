# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

### Development Setup
- `bash install` - Set up development environment with Python 3.13 virtual environment using uv
- `source activate` (or `. activate`) - Activate the virtual environment (symlinked to .venv/bin/activate or .venv/Scripts/activate on Windows)

### Testing
- `bash test` - Run tests using `uv run pytest tests/`
- `uv run pytest tests/integration/ -v --tb=short --maxfail=1` - Run integration tests sequentially (Docker conflicts prevention)
- `uv run pytest tests/ -n auto -vv` - Run tests directly with pytest (parallel execution)

### Linting and Code Quality
- `bash lint` - Run Python linting with ruff and pyright
- `uv run ruff check --fix src/ tests/` - Run ruff linting with auto-fixes
- `uv run ruff format src/ tests/` - Format code using ruff
- `uv run pyright` - Type checking with pyright

### Build and Package
- `uv pip install -e ".[dev]"` - Install package in editable mode with dev dependencies
- The package builds a wheel to `dist/clud-{version}-py3-none-any.whl`

### Docker
- `docker build -t niteris/clud .` - Build the development container image with Node.js/npm support
- `docker run --rm niteris/clud node --version` - Verify Node.js installation in container
- `docker run --rm niteris/clud npm --version` - Verify npm installation in container
- **Testing Docker builds**: Use a 10-minute timeout when testing docker build operations due to the complexity of the container setup

### Cleanup
- `bash clean` - Remove all build artifacts, caches, and virtual environment

## Architecture

### Purpose
`clud` is a Python CLI that runs Claude Code in "YOLO mode" by default, eliminating permission prompts for maximum development velocity. It provides both foreground and background Docker-based development environments.

### Project Structure
- `src/clud/` - Main package source code
- `src/clud/cli.py` - Main CLI entry point that routes to foreground/background agents
- `src/clud/agent_foreground.py` - Handles foreground Claude execution (YOLO mode)
- `src/clud/agent_background.py` - Manages Docker container operations
- `src/clud/docker/` - Docker management utilities
- `tests/` - Unit and integration tests using pytest
- `pyproject.toml` - Modern Python packaging configuration

### Key Components
- **CLI Router** (`cli.py`): Determines execution mode (foreground YOLO, Docker shell, UI mode, etc.)
- **Foreground Agent** (`agent_foreground.py`): Direct Claude Code execution with `--dangerously-skip-permissions`
- **Background Agent** (`agent_background.py`): Docker container management for isolated development
- **Docker Manager** (`docker/docker_manager.py`): Container lifecycle and resource management
- **Git Worktree Support** (`git_worktree.py`): Manage Git worktrees inside containers
- **Task System** (`task.py`): File-based task execution system
- **Agent Completion Detection** (`agent_completion.py`): Monitors terminal for idle detection

### Package Configuration
- Uses setuptools with pyproject.toml for modern Python packaging
- Entry points:
  - `clud` → `clud.cli:main`
  - `clud-bg` → `clud.cli_bg:main`
  - `clud-fb` → `clud.cli_fb:main`
- Supports Python 3.13+
- Key dependencies: docker, keyring, fasteners, httpx, pywinpty (Windows)

### Docker Container Architecture
- Base image: Ubuntu 24.04 with Python 3.13, Node.js 22, and development tools
- Includes Claude CLI with dangerous permissions alias (`clud`)
- Code-server for browser-based development (port 8743)
- Container sync mechanism for workspace isolation
- MCP server support via npm packages
- Entrypoint script manages container initialization

### Development Tools
- **uv** - Fast Python package installer and virtual environment manager
- **ruff** - Fast Python linter and formatter (configured for 200 char line length)
- **pyright** - Type checker with strict mode
- **pytest** - Testing framework with xdist for parallel execution

### Code Quality Configuration
- Ruff configured with 200-character line length and Python 3.13 target
- Ruff handles import sorting and unused import removal automatically
- Pyright configured for strict type checking with Python 3.13
- Integration tests run sequentially to avoid Docker conflicts

### Windows Compatibility
- The project is designed to work on Windows using git-bash
- UTF-8 encoding handling in all shell scripts
- pywinpty dependency for Windows terminal support

### Docker Development Guidelines
- Place new Docker dependencies towards the bottom of Dockerfile for faster iteration
- Docker's layer caching allows faster rebuilds when testing optional dependencies
- Container uses `/workspace` as the isolated working directory
- Host files mounted to `/host` directory read-only

## Code Quality Standards

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
- Strict type checking is enforced via pyright
- All functions should have proper return type annotations
- Use specific types rather than `Any` when possible
- Docker API types may show warnings due to third-party library issues (acceptable)