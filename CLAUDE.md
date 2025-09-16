# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

### Development Setup
- `bash install` - Set up development environment with Python 3.13 virtual environment using uv
- `source activate` (or `. activate`) - Activate the virtual environment (symlinked to .venv/bin/activate or .venv/Scripts/activate on Windows)

### Testing
- `bash test` - Run tests using `uv run pytest tests/`
- `uv run pytest tests/ -n auto -vv` - Run tests directly with pytest (parallel execution)

### Linting and Code Quality
- `bash lint` - Run Python linting with ruff and pyright
- `uv run ruff check --fix src/ tests/` - Run ruff linting with auto-fixes
- `uv run ruff format src/ tests/` - Format code using ruff
- `uv run pyright` - Type checking with pyright

### Build and Package
- `uv pip install -e ".[dev]"` - Install package in editable mode with dev dependencies
- The package builds a wheel to `dist/clud-0.0.1-py3-none-any.whl`

### Docker
- `docker build -t niteris/clud .` - Build the development container image with Node.js/npm support
- `docker run --rm niteris/clud node --version` - Verify Node.js installation in container
- `docker run --rm niteris/clud npm --version` - Verify npm installation in container
- **Testing Docker builds**: Use a 10-minute timeout when testing docker build operations due to the complexity of the container setup

### Cleanup
- `bash clean` - Remove all build artifacts, caches, and virtual environment

## Architecture

### Project Structure
This is a Python CLI package with the following structure:
- `src/clud/` - Main package source code
- `src/clud/cli.py` - CLI entry point (currently a stub that prints "Replace with a CLI entry point.")
- `tests/` - Unit tests using unittest framework
- `pyproject.toml` - Modern Python packaging configuration

### Package Configuration
- Uses setuptools with pyproject.toml for modern Python packaging
- Entry point: `clud` command maps to `clud.cli:main`
- Supports Python 3.13+
- Dev dependencies defined in `[project.optional-dependencies]`

### Development Tools
- **uv** - Fast Python package installer and virtual environment manager
- **ruff** - Fast Python linter and formatter (configured for 200 char line length, includes import management)
- **pyright** - Type checker
- **pytest** - Testing framework with xdist for parallel execution

### Code Quality Configuration
- Ruff configured with 200-character line length and Python 3.13 target
- Ruff handles import sorting and unused import removal automatically
- Pyright configured for type checking with Python 3.13

### Windows Compatibility
The project is designed to work on Windows using git-bash, with UTF-8 encoding handling in all shell scripts.

### Docker Development Guidelines
- When adding new Docker dependencies, place them towards the bottom of the Dockerfile to increase iteration speed during development
- Docker's layer caching means changes to later layers don't invalidate earlier cached layers
- This allows faster rebuilds when testing new optional dependencies

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