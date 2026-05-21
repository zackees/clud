# CLAUDE.md

Guidance for Claude Code when working in this repository.

**This file is an index.** Per-directory `README.md` files carry the real detail — descend into them as needed instead of expanding this file.

## Quick Reference

### Essential Commands

- **Build**: `bash build` — dev wheel (Rust binary + Python package)
- **Lint**: `bash lint` — `cargo fmt`, `cargo clippy`, `ruff` (**MANDATORY** after any code edit)
- **Test**: `bash test` — Rust unit tests + Python unit tests
- **Test (full)**: `bash test --integration` — adds integration tests with mock agents

### Soldr (Rust toolchain wrapper)

All `cargo` / `rustc` / `rustfmt` calls **must go through [soldr](https://github.com/zackees/soldr)**: `soldr cargo build`, `soldr cargo test -p clud-bin`, etc. soldr resolves the rustup-managed toolchain via `rustup which`, sidestepping chocolatey cargo on Windows and other stale PATH shims. A `.claude/hooks/check-soldr.py` PreToolUse hook enforces this.

Install soldr: `./install` (puts it in this repo's `.venv`) or `./install --global` (puts it in `~/.cargo/bin` or `~/.local/bin`). CI uses `zackees/setup-soldr@v0`.

## Repository Map

This is a Rust CLI (`clud`) distributed as a Python wheel via maturin (`bindings = "bin"`). The Rust source lives under `crates/` and is mirrored by a progressive-disclosure README tree:

```
crates/                    → see crates/README.md
  clud-bin/                → see crates/clud-bin/README.md
    src/                   → see crates/clud-bin/src/README.md
      command/             → see crates/clud-bin/src/command/README.md
      daemon/              → see crates/clud-bin/src/daemon/README.md
      dnd/                 → see crates/clud-bin/src/dnd/README.md
      voice/               → see crates/clud-bin/src/voice/README.md
    tests/                 → see crates/clud-bin/tests/README.md
    assets/                → see crates/clud-bin/assets/README.md
      skills/              → see crates/clud-bin/assets/skills/README.md
        clud-issue/        → see .../clud-issue/README.md
        clud-issue-triage/ → see .../clud-issue-triage/README.md
        clud-pr/           → see .../clud-pr/README.md
        clud-tag-release/  → see .../clud-tag-release/README.md
testbins/                  → see testbins/README.md
  mock-agent/              → see testbins/mock-agent/README.md
    src/                   → see testbins/mock-agent/src/README.md
src/clud/__init__.py       # Minimal Python package (version shim only)
ci/                        # CI scripts (env, build, lint, test)
tests/                     # Python tests (unit + integration)
```

### How to navigate

- **Where is X implemented?** Start at [`crates/clud-bin/src/README.md`](crates/clud-bin/src/README.md). It groups every top-level `.rs` file by concern (CLI surface, console/terminal, loop subsystem, GC, platform glue) and includes a "Quick lookup — which file owns a given subcommand" table.
- **How does a subsystem work?** Each subdirectory README (`command/`, `daemon/`, `dnd/`, `voice/`) describes its purpose, files, key public items with `file:line` refs, and who calls into it.
- **How does a test work?** [`crates/clud-bin/tests/README.md`](crates/clud-bin/tests/README.md) for Rust integration tests; [`testbins/mock-agent/README.md`](testbins/mock-agent/README.md) for the mock backend.
- **How do bundled skills ship?** [`crates/clud-bin/assets/skills/README.md`](crates/clud-bin/assets/skills/README.md) — note the two-installer caveat (`skills.rs` vs `skill_install.rs`).

## Key Design Decisions

- **YOLO by default**: always injects `--dangerously-skip-permissions` unless `--safe`.
- **Backend agnostic**: supports both `claude` and `codex` via `--claude` / `--codex`.
- **Unknown flag passthrough**: unrecognized CLI flags are forwarded to the backend.
- **Single `LaunchPlan`**: every code path goes through `command::build_launch_plan` (see [`src/command/README.md`](crates/clud-bin/src/command/README.md)). `--dry-run` emits this plan as JSON.
- **Test-first**: every feature has both Rust `#[test]` and Python subprocess tests.

## Code Quality Standards

After **any** code edit you **must** run `bash lint` (runs `cargo fmt --check`, `cargo clippy -D warnings`, and `ruff check`).

## Test Coverage

- ~104 Rust unit tests across arg parsing, command building, backend resolution, loop-spec (URL classification, GH-JSON parsing, marker files).
- ~21 Python unit tests via `--dry-run` subprocess calls.
- Python integration tests run end-to-end against [`mock-agent`](testbins/mock-agent/README.md), including the `clud loop` DONE/BLOCKED marker contract.

## CI Matrix

6 platforms × 4 job types = 24 GitHub Actions jobs:
- Linux x86 (`ubuntu-24.04`) + ARM (`ubuntu-24.04-arm`)
- Windows x86 (`windows-2025`) + ARM (`windows-11-arm`)
- macOS ARM (`macos-15`) + x86 (`macos-15-intel`)
