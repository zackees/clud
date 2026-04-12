# clud

Fast Rust CLI for running Claude Code and Codex in YOLO mode.

| Platform | Build | Lint | Unit Test | Integration Test |
|----------|-------|------|-----------|------------------|
| Linux x86 | [![Build](https://github.com/zackees/clud/actions/workflows/linux-x86-build.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-x86-build.yml) | [![Lint](https://github.com/zackees/clud/actions/workflows/linux-x86-lint.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-x86-lint.yml) | [![Unit Test](https://github.com/zackees/clud/actions/workflows/linux-x86-unit-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-x86-unit-test.yml) | [![Integration Test](https://github.com/zackees/clud/actions/workflows/linux-x86-integration-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-x86-integration-test.yml) |
| Linux ARM | [![Build](https://github.com/zackees/clud/actions/workflows/linux-arm-build.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-arm-build.yml) | [![Lint](https://github.com/zackees/clud/actions/workflows/linux-arm-lint.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-arm-lint.yml) | [![Unit Test](https://github.com/zackees/clud/actions/workflows/linux-arm-unit-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-arm-unit-test.yml) | [![Integration Test](https://github.com/zackees/clud/actions/workflows/linux-arm-integration-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-arm-integration-test.yml) |
| Windows x86 | [![Build](https://github.com/zackees/clud/actions/workflows/windows-x86-build.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-x86-build.yml) | [![Lint](https://github.com/zackees/clud/actions/workflows/windows-x86-lint.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-x86-lint.yml) | [![Unit Test](https://github.com/zackees/clud/actions/workflows/windows-x86-unit-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-x86-unit-test.yml) | [![Integration Test](https://github.com/zackees/clud/actions/workflows/windows-x86-integration-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-x86-integration-test.yml) |
| Windows ARM | [![Build](https://github.com/zackees/clud/actions/workflows/windows-arm-build.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-arm-build.yml) | [![Lint](https://github.com/zackees/clud/actions/workflows/windows-arm-lint.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-arm-lint.yml) | [![Unit Test](https://github.com/zackees/clud/actions/workflows/windows-arm-unit-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-arm-unit-test.yml) | [![Integration Test](https://github.com/zackees/clud/actions/workflows/windows-arm-integration-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-arm-integration-test.yml) |
| macOS x86 | [![Build](https://github.com/zackees/clud/actions/workflows/macos-x86-build.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-x86-build.yml) | [![Lint](https://github.com/zackees/clud/actions/workflows/macos-x86-lint.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-x86-lint.yml) | [![Unit Test](https://github.com/zackees/clud/actions/workflows/macos-x86-unit-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-x86-unit-test.yml) | [![Integration Test](https://github.com/zackees/clud/actions/workflows/macos-x86-integration-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-x86-integration-test.yml) |
| macOS ARM | [![Build](https://github.com/zackees/clud/actions/workflows/macos-arm-build.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-arm-build.yml) | [![Lint](https://github.com/zackees/clud/actions/workflows/macos-arm-lint.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-arm-lint.yml) | [![Unit Test](https://github.com/zackees/clud/actions/workflows/macos-arm-unit-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-arm-unit-test.yml) | [![Integration Test](https://github.com/zackees/clud/actions/workflows/macos-arm-integration-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-arm-integration-test.yml) |

## Install

```bash
pip install clud
```

## Usage

```bash
# Run Claude with a prompt (YOLO mode, no permission prompts)
clud -p "fix the failing tests"

# Send a one-off message
clud -m "what does this function do?"

# Continue last session
clud -c

# Use Codex instead of Claude
clud --codex -p "refactor this module"

# Choose a model
clud --model opus -p "review this PR"

# Autonomous loop (50 iterations by default)
clud loop "implement the feature described in TASK.md"
clud loop --loop-count 5 "fix all lint errors"

# Special workflows
clud up       # lint, test, commit
clud rebase   # rebase on main, resolve conflicts
clud fix      # fix all lint/test errors

# Disable YOLO mode
clud --safe -p "delete the database"

# See what would be executed
clud --dry-run -p "hello"

# Pipe mode
echo "explain this error" | clud
```

## Development

```bash
bash build    # Build dev wheel
bash lint     # Lint (cargo fmt + clippy + ruff)
bash test     # Unit tests (Rust + Python)
bash test --integration  # Include integration tests
```

## License

BSD 3-Clause
