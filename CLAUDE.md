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

#### Performance benchmarks

Standalone, opt-in performance harnesses live in [`bench/README.md`](bench/README.md).
They are not pytest tests; use the idle CPU runbook there when validating an
end-to-end daemon/client performance change.

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

### Cross-cutting registries — extend in all required places

Several features have a "single source of truth" registry that must be updated alongside the code change. Forgetting any of these causes silent misbehavior (passthrough instead of dispatch) or surprising failures (banned-import lint, missing bundled file). The full list:

- **New top-level `Command` subcommand** → 3 places:
  1. `Command` enum variant in `crates/clud-bin/src/args.rs`.
  2. Dispatch arm in `crates/clud-bin/src/main.rs`.
  3. **`subcommands: &[&str]` array in `args.rs::split_known_unknown` (~line 611)** — *gotcha*: a hardcoded list the unknown-flag-passthrough splitter uses; a missing entry routes your subcommand's argv to the backend agent as passthrough instead of dispatching it, and you get errors from the wrong layer (e.g., the backend complaining about your `--cmd` flag). Also extend `value_flags` / `bool_flags` arrays in the same function if your subcommand introduces new flags.

- **New bundled skill** (`crates/clud-bin/assets/skills/*/SKILL.md`) → `BUNDLED_SKILLS` registry in `crates/clud-bin/src/skills.rs`; frontmatter must parse via a real YAML parser. Guardrail tests: `soldr cargo test -p clud --lib skills::`. Same applies to root `skills/*/SKILL.md` via `crates/clud-bin/src/skill_install.rs` (`soldr cargo test -p clud --lib skill_install::`).

- **New bundled tool / hook** (`crates/clud-bin/assets/tools/<group>/*.py`) → `BUNDLED_TOOLS` array in `crates/clud-bin/src/tools.rs` with `include_str!` of the asset. Add a `bundled_includes_<tool>` guardrail test mirroring the existing ones (e.g. `bundled_includes_pr_merge_watch`, `bundled_includes_telemetry_hook`) so a future rename or removal doesn't silently break consumers. When retiring a managed bundled tool after users may have installed it, remove the bundle entry and add its old relative path to `PURGED_TOOLS` in `crates/clud-bin/src/tool_install.rs`; the purge only deletes files that still carry the `managed-by: clud` marker.

- **Test that needs raw `std::process::Command`** → add the test filename to the exempt set in `ci/banned_imports.py`. The lint enforces that production subprocess execution goes through `running_process::NativeProcess`; exemptions exist for tests that deliberately need raw spawning because `NativeProcess` would attach a `Containment::Contained` Job Object that masks what's being tested. If your test errors with `BANNED — use running_process::NativeProcess instead`, decide whether `NativeProcess` would distort the test; if yes, add yourself to the exempt set with a comment explaining why.

## Test Coverage

- ~1100+ Rust tests (unit + integration) across arg parsing, command building, backend resolution, loop-spec, daemon HTTP, registry guardrails, and end-to-end flows.
- ~185 Python tests, mostly `--dry-run` subprocess calls plus a smaller integration set.
- Python integration tests run end-to-end against [`mock-agent`](testbins/mock-agent/README.md), including the `clud loop` DONE/BLOCKED marker contract.

## CI Matrix

6 platforms × 4 job types = 24 GitHub Actions jobs.

Job types (reusable workflows under `.github/workflows/_*.yml`):
- `_lint.yml` — `bash lint`
- `_unit-test.yml` — `bash test` (no `--integration`)
- `_integration-test.yml` — `bash test --integration` (mock-agent end-to-end)
- `_build.yml` — wheel build

Platforms:
- Linux x86 (`ubuntu-24.04`) + ARM (`ubuntu-24.04-arm`)
- Windows x86 (`windows-2025`) + ARM (`windows-11-arm`)
- macOS ARM (`macos-15`) + x86 (`macos-15-intel`)
