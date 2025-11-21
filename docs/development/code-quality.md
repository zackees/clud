# Code Quality Standards

This document outlines the mandatory code quality standards for the `clud` project.

## Linting Requirement

**MANDATORY**: After ANY code editing (creating, modifying, or deleting Python files), you MUST run:

```bash
bash lint
```

This ensures all code changes pass:
- Ruff linting
- Pyright type checking

The lint check must pass successfully - address all errors and warnings before marking work as done.

This applies to all Python code in `src/` and `tests/` directories.

## Exception Handling

### General Rules

- **NEVER** use bare `except Exception: pass` or similar patterns that silently ignore exceptions
- All caught exceptions MUST be logged at minimum with appropriate context
- Use specific exception types when possible rather than catching broad `Exception`
- If an exception truly needs to be suppressed, use `contextlib.suppress()` and document why

### Example of Proper Exception Handling

```python
try:
    risky_operation()
except SpecificException as e:
    logger.warning(f"Operation failed with expected error: {e}")
    # Handle or re-raise as appropriate
```

### CRITICAL: KeyboardInterrupt Handling

**NEVER** silently catch or suppress `KeyboardInterrupt` exceptions.

#### Recommended: Use `handle_keyboard_interrupt()` Utility

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

#### Manual Handling

When `handle_keyboard_interrupt()` isn't suitable:

```python
try:
    operation()
except KeyboardInterrupt:
    raise  # MANDATORY: Always re-raise KeyboardInterrupt
except Exception as e:
    logger.error(f"Operation failed: {e}")
```

#### Why This Matters

- KeyboardInterrupt (Ctrl+C) is a user signal to stop execution
- Suppressing it creates unresponsive processes
- This applies to ALL exception handlers, including hook handlers, cleanup code, and background tasks

#### The `handle_keyboard_interrupt()` Utility

Features:
- Ensures KeyboardInterrupt is ALWAYS re-raised
- Optionally calls cleanup function before re-raising
- Handles cleanup failures gracefully (logs but doesn't suppress interrupt)
- Optionally logs the interrupt with custom message
- See `src/clud/util.py` and `tests/test_util.py` for implementation and examples

#### Linter Support

- Ruff's **BLE001** (blind-except) rule can detect overly broad exception handlers that catch `KeyboardInterrupt`
- BLE001 is NOT active by default - must be explicitly enabled with `--select BLE001` or in pyproject.toml
- Consider enabling BLE001 in the future for automatic detection of this pattern
- Currently relying on manual code review for KeyboardInterrupt handling

## Python Path Management

**NEVER** use `sys.path.insert()` or any other `sys.path` manipulation.

### Why?

Path problems are typically caused by trying to directly execute package code instead of using proper tools.

### The Right Way

**ALWAYS** use `uv run` for running Python scripts that need access to package dependencies:

```bash
# Good
uv run python script.py

# Bad - may cause import errors
python script.py
```

### Rules

- If you encounter import errors, the solution is to use `uv run`, not to modify `sys.path`
- `sys.path` imports before regular imports are strictly forbidden and should be flagged as code quality violations

## Type Annotations

### Return Type Annotations

Enforced via ruff's ANN ruleset (flake8-annotations).

**MANDATORY**: All functions must have explicit return type annotations:

```python
# Good
def get_user(id: int) -> User:
    ...

def process_data(data: str) -> None:
    ...

# Bad - missing return type
def get_user(id: int):
    ...
```

This includes:
- Public functions (ANN201)
- Private functions (ANN202)
- Special methods like `__init__` (ANN204)

Function arguments must also have type annotations (ANN001).

`typing.Any` is allowed when necessary (ANN401 is ignored).

### Type Checking

Strict type checking is enforced via pyright:
- Use specific types rather than `Any` when possible
- `reportUnknownVariableType` and `reportUnknownArgumentType` are configured as **errors**

### Third-Party Library Amnesty

Errors from third-party libraries (keyring, telegram, etc.) should be given lint amnesty:
- These errors from external dependencies are acceptable and should NOT be "fixed" with type ignore comments
- Always fix type errors in code you control (`src/clud/` and `tests/`)
- Common acceptable errors from third-party: `reportUnknownVariableType`, `reportUnknownArgumentType` from keyring, telegram, etc.
- Do NOT add `# type: ignore` or `# pyright: ignore` comments for third-party library type issues
- **The goal**: zero unknown types in our code, but accept incomplete type stubs from dependencies

## Test Framework Standard

**MANDATORY**: All unit tests MUST use the `unittest` framework.

### Requirements

- All test files MUST have a `main` function that runs `unittest.main()`
- Tests are executed via pytest (which is compatible with unittest), but the test code itself must use unittest

### Example Test File Structure

```python
import unittest

class TestMyFeature(unittest.TestCase):
    def test_something(self) -> None:
        self.assertEqual(1, 1)

if __name__ == "__main__":
    unittest.main()
```

This allows tests to be run both via pytest and directly as Python scripts.

## Process Execution Standard

**MANDATORY**: Prefer `running-process` over `subprocess` for executing external commands.

### CRITICAL: Never Use `capture_output=True` for Long-Running Processes

**NEVER** use `subprocess.run()` with `capture_output=True` for long-running processes:

```python
# BAD - Can stall on long output!
result = subprocess.run(["lint-test"], capture_output=True)
```

**Why?**
- `capture_output=True` buffers stdout/stderr in memory
- Causes processes to **stall** when buffers fill
- This is especially problematic for commands like `lint-test`, `pytest`, or any process with substantial output

### The Right Way: Use `RunningProcess.run_streaming()`

**ALWAYS** use `RunningProcess.run_streaming()` for commands that may produce significant output:

```python
from running_process import RunningProcess

# Good: Streaming output for long-running processes
returncode = RunningProcess.run_streaming(["lint-test"])
```

Benefits:
- Streams output to console in real-time without buffering
- Prevents stdout/stderr buffer stalls
- Provides better user experience with live output

### When to Use subprocess

Only for simple, short-lived commands where you need to capture a small amount of output.

### Why This Matters

- Stdout/stderr buffers have limited capacity (typically 64KB)
- When full, the process blocks until the buffer is read
- With `capture_output=True`, the buffer is only read after the process completes
- This creates a deadlock for processes with large output

## Playwright E2E Testing Protocol

### File Naming

- E2E tests: `tests/test_*_e2e.py`
- Excluded from pyright type checking

### Run Command

```bash
bash test --full
```

Auto-installs Playwright browsers.

### Unique Ports

Each E2E test must use a unique port (e.g., 8899, 8902, 8903) to avoid conflicts.

### Server Lifecycle

- Start in `setUpClass()`
- Stop in `tearDownClass()`
- Use `CLUD_NO_BROWSER=1` env var

### Console Error Filtering

- Ignore "WebSocket" and "favicon" errors
- Fail on all other console errors

### Test Artifacts

Save screenshots/reports to `tests/artifacts/` (git-ignored).

### Standard Template

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

## Code Quality Tools

### Ruff

- 200-character line length
- Python 3.13 target
- Handles import sorting and unused import removal automatically

```bash
# Check with auto-fixes
uv run ruff check --fix src/ tests/

# Format code
uv run ruff format src/ tests/
```

### Pyright

- Strict mode enabled
- Type checking with Python 3.13

```bash
uv run pyright
```

## Related Documentation

- [Development Setup](setup.md)
- [Architecture](architecture.md)
- [Troubleshooting](troubleshooting.md)
