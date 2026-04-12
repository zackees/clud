# clud

Fast Rust CLI for running Claude Code and Codex in YOLO mode.

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
