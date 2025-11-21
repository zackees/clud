# Development Setup

This guide covers setting up the development environment for `clud`.

## Prerequisites

- Python 3.13+
- Git
- Node.js and npm (for frontend development)

## Initial Setup

### Install Development Environment

```bash
bash install
```

This command:
- Sets up Python 3.13 virtual environment using `uv`
- Installs all development dependencies
- Configures the project for development

### Activate Virtual Environment

```bash
source activate
# or
. activate
```

The `activate` script is symlinked to:
- `.venv/bin/activate` (Unix/Linux/macOS)
- `.venv/Scripts/activate` (Windows)

## Testing

### Run Unit Tests

```bash
bash test
```

- Runs unit tests (excludes E2E tests by default)
- Uses pytest with parallel execution

### Run Full Test Suite

```bash
bash test --full
```

- Includes Playwright E2E tests
- Automatically installs Playwright browsers with system dependencies
- Tests Web UI loading and verifies no console errors
- Takes longer than unit tests
- Recommended before releases
- Test artifacts (screenshots, reports) are stored in `tests/artifacts/` (git-ignored)

### Run Tests Directly with pytest

```bash
uv run pytest tests/ -n auto -vv
```

## Linting and Code Quality

### Run All Linting Checks

```bash
bash lint
```

Runs both ruff linting and pyright type checking.

**MANDATORY**: After ANY code editing (creating, modifying, or deleting Python files), you MUST run `bash lint`.

### Ruff Linting

```bash
# Check for issues with auto-fixes
uv run ruff check --fix src/ tests/

# Format code
uv run ruff format src/ tests/
```

### Type Checking

```bash
uv run pyright
```

## Build and Package

### Install in Editable Mode

```bash
uv pip install -e ".[dev]"
```

### Build Wheel

```bash
# Build process creates wheel in dist/
# dist/clud-{version}-py3-none-any.whl
```

## Frontend Development

The Web UI uses Svelte 5 + SvelteKit + TypeScript.

### Install Frontend Dependencies

```bash
cd src/clud/webui/frontend
npm install
```

### Run Development Server

```bash
cd src/clud/webui/frontend
npm run dev
```

- Starts dev server with hot reload
- Default port: 5173

### Build for Production

```bash
cd src/clud/webui/frontend
npm run build
```

- Outputs to `build/` directory
- Uses `@sveltejs/adapter-static` for SPA mode

### Preview Production Build

```bash
cd src/clud/webui/frontend
npm run preview
```

### Type-Check Svelte Components

```bash
cd src/clud/webui/frontend
npm run check
```

## Cleanup

### Remove Build Artifacts

```bash
bash clean
```

Removes:
- All build artifacts
- Caches
- Virtual environment

## Development Tools

- **uv** - Fast Python package installer and virtual environment manager
- **ruff** - Fast Python linter and formatter (200 char line length)
- **pyright** - Type checker with strict mode
- **pytest** - Testing framework with xdist for parallel execution
- **Playwright** - Browser automation for E2E testing (Chromium headless)

## Windows Compatibility

The project is designed to work on Windows using git-bash:
- UTF-8 encoding handling in all shell scripts
- pywinpty dependency for Windows terminal support
- Path handling works across Unix and Windows

## Next Steps

- Review [Code Quality Standards](code-quality.md)
- Explore [Architecture](architecture.md)
- Check [Troubleshooting](troubleshooting.md) if you encounter issues
