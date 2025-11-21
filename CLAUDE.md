# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Quick Reference

### Essential Commands

- **Setup**: `bash install` - Set up development environment
- **Activate**: `source activate` or `. activate` - Activate virtual environment
- **Test**: `bash test` - Run unit tests
- **Test (Full)**: `bash test --full` - Run full test suite including E2E tests
- **Lint**: `bash lint` - **MANDATORY** after any code changes
- **Clean**: `bash clean` - Remove all build artifacts

### Documentation Index

- **[Development Setup](docs/development/setup.md)** - Installation, testing, linting, build
- **[Architecture](docs/development/architecture.md)** - Project structure, key components
- **[Code Quality](docs/development/code-quality.md)** - Standards, conventions, requirements
- **[Troubleshooting](docs/development/troubleshooting.md)** - Claude Code installation issues

## Features

### Core Features

- **[Pipe Mode](docs/features/pipe-mode.md)** - Unix-style I/O piping support
- **[Cron Scheduler](docs/features/cron-scheduler.md)** - Automated task scheduling
- **[Web UI](docs/features/webui.md)** - Browser-based interface
- **[Terminal Console](docs/features/terminal.md)** - Integrated terminal in Web UI
- **[Backlog Tab](docs/features/backlog.md)** - Task visualization from Backlog.md

### Integration Features

- **[Hooks & Message Handler API](docs/features/hooks.md)** - Event-based architecture
- **[Telegram Bot API](docs/features/telegram-api.md)** - Telegram integration and testing

## Code Quality Standards (Summary)

### MANDATORY: Linting Requirement

After **ANY** code editing (creating, modifying, or deleting Python files), you **MUST** run:

```bash
bash lint
```

This ensures all code changes pass ruff linting and pyright type checking.

### Critical Standards

- **Exception Handling**: Never silently catch exceptions; always log with context
- **KeyboardInterrupt**: Use `handle_keyboard_interrupt()` utility or always re-raise
- **Python Path**: NEVER use `sys.path.insert()`; use `uv run` instead
- **Type Annotations**: All functions must have explicit return type annotations
- **Process Execution**: Prefer `running-process` over `subprocess`; use `RunningProcess.run_streaming()` for long-running processes
- **Test Framework**: All unit tests MUST use `unittest` framework
- **E2E Tests**: Unique ports per test, exclude from pyright type checking

See [Code Quality Standards](docs/development/code-quality.md) for complete details.

## Project Purpose

`clud` is a Python CLI that runs Claude Code in "YOLO mode" by default, eliminating permission prompts for maximum development velocity.

### Entry Point

- `clud` → `clud.cli:main`

### Key Dependencies

- **keyring** - Secure credential storage
- **httpx** - HTTP client for API calls
- **pywinpty** - Windows terminal support (Windows only)
- **running-process** - Process execution utilities
- **fastapi** - Web framework for APIs
- **python-telegram-bot** - Telegram bot integration

## Development Workflow

1. **Read** [Development Setup](docs/development/setup.md) for environment setup
2. **Review** [Architecture](docs/development/architecture.md) to understand project structure
3. **Follow** [Code Quality Standards](docs/development/code-quality.md) when writing code
4. **Run** `bash lint` after **any** code changes (**MANDATORY**)
5. **Test** with `bash test` (unit) or `bash test --full` (E2E)
6. **Refer** to [Troubleshooting](docs/development/troubleshooting.md) if issues arise

## Platform Support

- **Linux/macOS**: Full support with native tools
- **Windows**: Designed to work with git-bash
  - UTF-8 encoding handling
  - pywinpty for terminal support
  - Cross-platform path handling

## Related Resources

- **Existing Documentation**:
  - `docs/telegram-integration.md` - Telegram integration details
  - `docs/telegram-webapp-design.md` - Telegram webapp design
- **Example Cron Tasks**:
  - `examples/cron/daily-backup.md`
  - `examples/cron/hourly-report.md`

---

**Remember**: Always run `bash lint` after editing Python files!
