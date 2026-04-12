# CLAUDE.md

This file provides guidance to Claude Code when working with this repository.

## Quick Reference

### Essential Commands

- **Build**: `bash build` - Build dev wheel (Rust binary + Python package)
- **Lint**: `bash lint` - Run cargo fmt, clippy, and ruff (**MANDATORY** after code changes)
- **Test**: `bash test` - Run Rust unit tests + Python unit tests
- **Test (Full)**: `bash test --integration` - Include integration tests with mock agents

### Architecture

This is a Rust CLI (`clud`) distributed as a Python wheel via maturin `bindings = "bin"`.

```
crates/clud-bin/          # Main Rust binary crate
  src/
    main.rs               # Entry point: pipe detection, dry-run, execution
    args.rs               # clap argument parsing with passthrough support
    backend.rs            # Backend discovery (claude/codex on PATH)
    command.rs            # Command builder: YOLO injection, prompts, loops
testbins/mock-agent/      # Mock agent for integration testing
src/clud/__init__.py      # Minimal Python package (version only)
ci/                       # CI scripts (env, build, lint, test)
tests/                    # Python tests (unit + integration)
```

### Key Design Decisions

- **YOLO by default**: Always injects `--dangerously-skip-permissions` unless `--safe`
- **Backend agnostic**: Supports both `claude` and `codex` via `--claude`/`--codex` flags
- **Unknown flag forwarding**: Unrecognized CLI flags pass through to the backend
- **Test-first**: Every feature has both Rust `#[test]` and Python subprocess tests
- **`--dry-run`**: Outputs JSON with the command that would be executed

### Code Quality Standards

After **ANY** code editing, you **MUST** run:

```bash
bash lint
```

This runs `cargo fmt --check`, `cargo clippy -D warnings`, and `ruff check`.

### Test Coverage

- **34 Rust unit tests**: arg parsing, command building, backend resolution
- **15 Python unit tests**: CLI output via `--dry-run` subprocess calls
- **18 Python integration tests**: end-to-end with mock claude/codex agents

### CI Matrix

6 platforms x 4 job types = 24 GitHub Actions jobs:
- Linux x86 (`ubuntu-24.04`) + ARM (`ubuntu-24.04-arm`)
- Windows x86 (`windows-2025`) + ARM (`windows-11-arm`)
- macOS ARM (`macos-15`) + x86 (`macos-15-intel`)
