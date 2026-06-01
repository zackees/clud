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
docs/                      → see docs/README.md
  ARCHITECTURE.md          # index of subsystem topic docs
  DESIGN_DECISIONS.md      # ADR-style records (DD-001 … DD-010)
  architecture/            # one file per cross-cutting subsystem
src/clud/__init__.py       # Minimal Python package (version shim only)
ci/                        # CI scripts (env, build, lint, test)
tests/                     # Python tests (unit + integration)
```

### How to navigate

- **Where is X implemented?** Start at [`crates/clud-bin/src/README.md`](crates/clud-bin/src/README.md). It groups every top-level `.rs` file by concern and includes a "Quick lookup — which file owns a given subcommand" table.
- **What's in this directory?** Each directory's `README.md` lists its files, key public items with `file:line` refs, and who calls into it.
- **How does a subsystem work end-to-end?** [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — topic docs that span multiple directories (loop, daemon IPC, session lifecycle, skill system, gc/registry, Windows quirks, launch plan).
- **Why was it designed this way?** [`docs/DESIGN_DECISIONS.md`](docs/DESIGN_DECISIONS.md) — ADR-style rationale for non-obvious choices.
- **How does a test work?** [`crates/clud-bin/tests/README.md`](crates/clud-bin/tests/README.md) for Rust integration tests; [`testbins/mock-agent/README.md`](testbins/mock-agent/README.md) for the mock backend.

## Architecture & design docs

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — index of subsystem topic docs (each ~150–400 lines, self-contained).
- [`docs/DESIGN_DECISIONS.md`](docs/DESIGN_DECISIONS.md) — 10 ADRs covering the non-obvious choices below and more.

## Where to put new docs

Tiered to keep agent context windows small and prevent duplication:

1. **Per-directory README** (`<dir>/README.md`) covers **what's in this directory** — files, key types with `file:line`, callers. If a fact applies only inside one directory, write it here.
2. **Subsystem topic doc** (`docs/architecture/<topic>.md`) covers **how a subsystem works across directories**. If a concept spans 2+ directories or 3+ files, write it here and have the per-dir READMEs link in with a one-line breadcrumb.
3. **Design decision** (`docs/DESIGN_DECISIONS.md`, append-only `DD-NNN`) covers **why** a non-obvious choice was made. If a reader could plausibly ask "why didn't you do it the other way?", add a DD.
4. **Never duplicate.** One doc owns each fact; everyone else links. When you find yourself copying a paragraph, replace the copy with a breadcrumb.

For a new cross-cutting feature: add the topic doc → register it in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) → add a breadcrumb in each touched per-dir README → if the design is non-obvious, append a `DD-NNN` to `DESIGN_DECISIONS.md`.

## Key Design Decisions (summary)

See [`docs/DESIGN_DECISIONS.md`](docs/DESIGN_DECISIONS.md) for full rationale.

- **YOLO by default** — `--dangerously-skip-permissions` is auto-injected unless `--safe` ([DD-002](docs/DESIGN_DECISIONS.md#dd-002-yolo-mode-is-the-default-safe-is-the-opt-out)).
- **Backend agnostic** — supports both `claude` and `codex` via `--claude` / `--codex` ([DD-004](docs/DESIGN_DECISIONS.md#dd-004-backend-agnostic--support-both-claude-and-codex)).
- **Single `LaunchPlan`** — every code path goes through `command::build_launch_plan`; `--dry-run` emits it as JSON ([DD-005](docs/DESIGN_DECISIONS.md#dd-005-single-launchplan-as-source-of-truth-for-everything-clud-runs), [launch-plan.md](docs/architecture/launch-plan.md)).
- **Unknown flag passthrough** — unrecognized CLI flags are forwarded to the backend.
- **Test-first** — every feature has both Rust `#[test]` and Python subprocess tests.

## Code Quality Standards

After **any** code edit you **must** run `bash lint` (runs `cargo fmt --check`, `cargo clippy -D warnings`, and `ruff check`).

### Bundled Skill Imports

When adding or renaming any imported/bundled skill, update the relevant `BUNDLED_SKILLS` registry and make sure the `SKILL.md` frontmatter parses with a real YAML parser. The guardrail tests live in `crates/clud-bin/src/skills.rs` for `crates/clud-bin/assets/skills/*/SKILL.md` imports and `crates/clud-bin/src/skill_install.rs` for root `skills/*/SKILL.md` imports. Run `soldr cargo test -p clud --lib skills::` and `soldr cargo test -p clud --lib skill_install::` after changing skill imports or frontmatter.

## Test Coverage

- ~104 Rust unit tests across arg parsing, command building, backend resolution, loop-spec (URL classification, GH-JSON parsing, marker files).
- ~21 Python unit tests via `--dry-run` subprocess calls.
- Python integration tests run end-to-end against [`mock-agent`](testbins/mock-agent/README.md), including the `clud loop` DONE/BLOCKED marker contract.

## CI Matrix

6 platforms × 4 job types = 24 GitHub Actions jobs:
- Linux x86 (`ubuntu-24.04`) + ARM (`ubuntu-24.04-arm`)
- Windows x86 (`windows-2025`) + ARM (`windows-11-arm`)
- macOS ARM (`macos-15`) + x86 (`macos-15-intel`)
