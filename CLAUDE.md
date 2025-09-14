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
- `docker build -t clud-dev .` - Build the development container image with Node.js/npm support
- `docker run --rm clud-dev node --version` - Verify Node.js installation in container
- `docker run --rm clud-dev npm --version` - Verify npm installation in container

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