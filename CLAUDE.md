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
- The package builds a wheel to `dist/clud-{version}-py3-none-any.whl`

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

## Architecture

### Purpose
`clud` is a Python CLI that runs Claude Code in "YOLO mode" by default, eliminating permission prompts for maximum development velocity.

### Project Structure
- `src/clud/` - Main package source code
- `src/clud/cli.py` - Main CLI entry point
- `src/clud/agent_foreground.py` - Handles Claude Code execution in YOLO mode
- `src/clud/task.py` - File-based task execution system
- `tests/` - Unit and integration tests using pytest
- `pyproject.toml` - Modern Python packaging configuration

### Key Components
- **CLI Router** (`cli.py`): Main entry point handling special commands and utility modes (fix, up)
- **Foreground Agent** (`agent_foreground.py`): Direct Claude Code execution with `--dangerously-skip-permissions`
- **Task System** (`task.py`): File-based task execution system
- **Agent Completion Detection** (`agent_completion.py`): Monitors terminal for idle detection

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
