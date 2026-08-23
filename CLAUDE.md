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
      test_runtime/        → see crates/clud-bin/src/test_runtime/README.md
      voice/               → see crates/clud-bin/src/voice/README.md
    tests/                 → see crates/clud-bin/tests/README.md
    assets/                → see crates/clud-bin/assets/README.md
      skills/              → see crates/clud-bin/assets/skills/README.md
        clud-issue/        → see .../clud-issue/README.md
        clud-issue-triage/ → see .../clud-issue-triage/README.md
        clud-tag-release/  → see .../clud-tag-release/README.md
testbins/                  → see testbins/README.md
  mock-agent/              → see testbins/mock-agent/README.md
    src/                   → see testbins/mock-agent/src/README.md
docs/                      → see docs/README.md
  ARCHITECTURE.md          # index of subsystem topic docs
  DESIGN_DECISIONS.md      # ADR-style records (DD-001 … DD-039)
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

- **YOLO by default** — the effective harness's permission-bypass flag is auto-injected unless `--safe` ([DD-002](docs/DESIGN_DECISIONS.md#dd-002-yolo-mode-is-the-default-safe-is-the-opt-out)).
- **Backend agnostic** — supports both `claude` and `codex` via `--claude` / `--codex` ([DD-004](docs/DESIGN_DECISIONS.md#dd-004-backend-agnostic--support-both-claude-and-codex)).
- **Single `LaunchPlan`** — production launches go through `command::build_launch_plan_for_target`; `build_launch_plan` is the native compatibility/test wrapper, and `--dry-run` emits the resolved plan as JSON ([DD-005](docs/DESIGN_DECISIONS.md#dd-005-single-launchplan-as-source-of-truth-for-everything-clud-runs), [launch-plan.md](docs/architecture/launch-plan.md)).
- **Unknown flag passthrough** — unrecognized CLI flags are forwarded to the backend.
- **Test-first** — every feature has both Rust `#[test]` and Python subprocess tests.
- **OpenRouter model selection** — OpenRouter is a gateway while Claude Code
  remains the frontend. `clud --openrouter` uses the reviewed Sonnet alias,
  `--model <wire-id>` pins startup, and Claude Code's `/model` shows the live
  gateway-discovered rows **alongside** its own built-in ones — discovery adds
  rows and cannot subtract them, so the picker is not constrainable from clud
  ([DD-054](docs/DESIGN_DECISIONS.md#dd-054-the-model-picker-belongs-to-the-harness-and-discovery-only-adds-rows)).
  Do not add a second clud-side picker or fold changing OpenRouter
  inventory into the static catalog. Preserve independent role-model mappings
  and treat non-Claude models as best-effort; see
  [`provider-selection.md`](docs/architecture/provider-selection.md#openrouter-model-selection-contract).

## Code Quality Standards

After **any** code edit you **must** run `bash lint` (runs `cargo fmt --check`, `cargo clippy -D warnings`, and `ruff check`).

### Process execution: `running-process` only

**Python's `subprocess` module is banned everywhere.** Do not import it, invoke it indirectly, or introduce any new exception. Python's blocking stdout/stderr reads have caused serious problems in the toolset.

**Rust's `std::process` APIs are banned for the same reason.** Do not use them to launch or manage processes. Use the `running-process` bindings exclusively in both languages: `running-process` provides efficient, non-blocking streamed stdout and stderr handling. Preserve that streaming behavior in all process-execution code.

The only narrowly permitted Rust exception is a test that must use raw `std::process::Command` because `running_process::NativeProcess` would change the behavior under test. Such a test requires a documented, filename-specific exemption in `ci/banned_imports.py`; do not add an exemption for ordinary tests or production code.

### Cross-cutting registries — extend in all required places

Several features have a "single source of truth" registry that must be updated alongside the code change. Forgetting any of these causes silent misbehavior (passthrough instead of dispatch) or surprising failures (banned-import lint, missing bundled file). The full list:

- **New top-level `Command` subcommand** → 3 places (4 if it takes a raw command):
  1. `Command` enum variant in `crates/clud-bin/src/args.rs`.
  2. Dispatch arm in `crates/clud-bin/src/main.rs`.
  3. **`subcommands: &[&str]` array in `args.rs::split_known_unknown` (~line 611)** — *gotcha*: a hardcoded list the unknown-flag-passthrough splitter uses; a missing entry routes your subcommand's argv to the backend agent as passthrough instead of dispatching it, and you get errors from the wrong layer (e.g., the backend complaining about your `--cmd` flag). Also extend `value_flags` / `bool_flags` arrays in the same function if your subcommand introduces new flags.
  4. **`SEPARATOR_OWNING_SUBCOMMANDS` in the same function** — *only* if your subcommand declares a `trailing_var_arg` argument taking a raw command after `--`. *Gotcha*: omitting it fails **silently**, not loudly — clud swallows everything after `--` as backend passthrough and your subcommand sees an empty argument vector, which reads as "the user passed no command". It was a single-valued constant while `tool run` was the only such parser; `test run` (#407) made it a list.

- **New bundled skill** (`crates/clud-bin/assets/skills/*/SKILL.md`) → `BUNDLED_SKILLS` registry in `crates/clud-bin/src/skills.rs`; frontmatter must parse via a real YAML parser. Guardrail tests: `soldr cargo test -p clud --lib skills::`. *Gotcha*: `assets/skills/` is the **only** source of truth, and `skills.rs` the only installer. clud used to have a second registry (`skill_install.rs`) with its own source tree at the repo root; both wrote the same `~/.claude/skills/` files, so each launch classified the other's output as drift, printed `updated /<name>`, and silently reverted the newer bodies ([DD-039](docs/DESIGN_DECISIONS.md#dd-039-bundled-skills-have-exactly-one-source-of-truth)). Do not add a second installer or a second skill tree — `ci/banned_skill_sources.py` fails `bash lint` if you do. When retiring a skill after users may have installed it, delete the asset dir, remove the bundle entry, and add its old name to `PURGED_BUNDLED_SKILLS` in the same file ([DD-040](docs/DESIGN_DECISIONS.md#dd-040-clud-pr-clud-fix-clud-do-and-clud-pr-merge-are-retired-in-favor-of-goal)); the purge only deletes a `SKILL.md` that still carries the `managed-by: clud` marker, and it sweeps every backend's skills dir, not just `~/.claude`.

- **New bundled tool / hook** (`crates/clud-bin/assets/tools/<group>/*.py`) → `BUNDLED_TOOLS` array in `crates/clud-bin/src/tools.rs` with `include_str!` of the asset. Add a `bundled_includes_<tool>` guardrail test mirroring the existing ones (e.g. `bundled_includes_pr_merge_watch`, `bundled_includes_telemetry_hook`) so a future rename or removal doesn't silently break consumers. When retiring a managed bundled tool after users may have installed it, remove the bundle entry and add its old relative path to `PURGED_TOOLS` in `crates/clud-bin/src/tool_install.rs`; the purge only deletes files that still carry the `managed-by: clud` marker.

- **Changing reap/spare logic** → decisions must be expressible against injected
  `ProcessFacts` (unit-testable, cross-platform). Add the case to the Tier 1
  decision table in `job_orphan_reaper`'s `lifecycle_tests` first, asserting
  **spare + reason** rather than just the outcome; reach for an integration test
  only if a real Job Object or real detachment is the thing under test (budget:
  ≤5). *Gotcha*: never conclude a daemon marker is unused by grepping this repo —
  `RUNNING_PROCESS_IS_DAEMON` is set by **other programs** (zccache, soldr) via
  `running-process`, and a draft of #673 nearly deleted the spare-list on exactly
  that reasoning. Daemon-stub tests need raw `std::process::Command` and must be
  added to `ci/banned_imports.py`'s exempt set per the bullet below, because
  `NativeProcess` would set the very marker whose absence is under test. See
  [`docs/architecture/process-reaping.md`](docs/architecture/process-reaping.md).

- **Test that needs raw `std::process::Command`** → add the test filename to the exempt set in `ci/banned_imports.py`. The lint enforces that production subprocess execution goes through `running_process::NativeProcess`; exemptions exist for tests that deliberately need raw spawning because `NativeProcess` would attach a `Containment::Contained` Job Object that masks what's being tested. If your test errors with `BANNED — use running_process::NativeProcess instead`, decide whether `NativeProcess` would distort the test; if yes, add yourself to the exempt set with a comment explaining why.

- **Bumping soldr** → 3 places, same commit: the exact pin in `pyproject.toml` (`build-system.requires = ["soldr==X.Y.Z"]`, the *build backend*), every `zackees/setup-soldr` version under `.github/` (CI's *toolchain*, including composite-action input defaults), and `VERSION="${SOLDR_VERSION:-X.Y.Z}"` in `./install` (a developer's local toolchain). These resolve independently — pinning only one leaves builds running a soldr that CI never tested, which is how a broken upstream patch release reddened `main` on branches pinned to a known-good version. `tests/test_packaging_metadata.py::test_soldr_versions_move_in_lockstep` asserts all three agree and rejects a non-exact requirement. One soldr version is deliberately *outside* that set and must be bumped by hand: `crates/clud-bin/assets/tools/docker/docker_build_soldr.py`'s `ARG SOLDR_VERSION`, which is guarded by a matching literal in `crates/clud-bin/src/tools.rs` (bump both together). See [DD-020](docs/DESIGN_DECISIONS.md#dd-020-the-soldr-build-backend-is-pinned-exactly-and-cis-toolchain-pin-is-asserted-to-match-it).

## Test Coverage

- ~1100+ Rust tests (unit + integration) across arg parsing, command building, backend resolution, loop-spec, daemon HTTP, registry guardrails, and end-to-end flows.
- ~185 Python tests, mostly `--dry-run` subprocess calls plus a smaller integration set.
- Python integration tests run end-to-end against [`mock-agent`](testbins/mock-agent/README.md), including the `clud loop` DONE/BLOCKED marker contract.

## CI

Build once per target triple **on Linux**, then execute the result on native
runners that have no Rust toolchain at all. Full design and rationale:
[`docs/architecture/ci.md`](docs/architecture/ci.md).

Entrypoint: `.github/workflows/ci.yml` (the only push/PR workflow).

| File | Role |
| --- | --- |
| `.github/actions/setup-build/action.yml` | python + uv + mold + soldr + cross tooling. Build side only. |
| `.github/actions/setup-exec/action.yml` | python + uv, and **removes** the Rust toolchain. Exec side only. |
| `.github/workflows/_build-target.yml` | one triple → one test bundle (+ optional wheel). The only workflow that compiles Rust. |
| `.github/workflows/_run-tests.yml` | one triple × one suite → execution, no compilation. |
| `.github/workflows/_dylint.yml` | Linux-only nightly lint; off the PR path. |
| `ci/ci_matrix.py` | the triple → {build host, cross strategy, exec runner} table. |
| `ci/xbuild.py` | every cargo/maturin invocation, plus the per-strategy cross env. |
| `ci/bundle.py` / `ci/run_bundle.py` | pack the bundle / execute it on the exec runner. |

Things that bite:

- **Only `auto-release.yml` may build `--release`.** `_build-target.yml` fails
  the job if any other workflow passes `profile: release`.
- **Never use `uv run` in a workflow step.** `pyproject.toml` sets
  `build-backend = "soldr"`, so `uv run` syncs the project and triggers a full
  PEP 517 maturin build. Use `$VENV_PY` (exported by both composite actions),
  which is what `lint` already does locally.
- **soldr owns Apple/MSVC cross builds (#637, #714).** Never invoke or install
  `cargo xwin`, `cargo zigbuild`, `zig cc`, `cross` or `osxcross` for a
  `*-apple-darwin` / `*-pc-windows-msvc` target — use `soldr prepare` /
  `soldr build`. As of soldr 0.8.40, soldr **also owns `*-unknown-linux-gnu`**:
  its catalogue GNU toolchain (`gcc-13.3.0-glibc-2.17-1`, soldr#2238) replaced
  zig for Linux and the manylinux wheel, so `ci/xbuild.py::is_soldr_owned`
  covers linux-gnu and `cargo_argv` refuses zigbuild for it too. clud no longer
  invokes zig anywhere. `ci/xbuild.py` still sets `WHISPER_LINK_CXX_STATIC` and
  appends static-libstdc++ RUSTFLAGS for the manylinux_2_17 C++ floor, a
  mechanism whisper-rs-sys needed; it is currently inert (whisper-rs was
  removed — see `crates/clud-bin/src/voice/README.md`) but harmless to leave,
  since no remaining dependency emits a dynamic libstdc++ link directive.
  `ci/banned_cross_tools.py` enforces the ban under `bash lint` and CI's static
  job; `ci/xbuild.py::cargo_argv` additionally *raises* on a zigbuild strategy
  for any soldr-owned triple (which is now every clud triple), because the text
  scan cannot follow a target held in a variable.
  Two rule classes, and the distinction is the thing to get right when editing
  it: **unconditional** — flagged wherever they appear, no target consulted:
  `cargo xwin`, the bare `xwin` CLI, `XWIN_*`, `osxcross`, `cross build`,
  `Cross.toml`, **and now every zig invocation** (`cargo zigbuild`, `maturin
  --zig`, `zig cc` — banned at every target since soldr#2299, because soldr's
  catalogue toolchain replaced zig's last Linux use) — versus
  **conditional on a soldr-owned triple** — now only the hand-rolled
  `[target.<triple>] linker =` TOML rule. Installs are matched against the whole file, so
  a `taiki-e/install-action` step with `tool: cargo-xwin` two lines below is
  caught, and an install suppresses the invocation rules on its own line so one
  mistake is not counted twice. Scope: `.github/`, `ci/`, `bench/`, `crates/`,
  `dylints/`, `testbins/`, `tests/`, `.claude/hooks/`, the root
  entrypoints and `.cargo/config.toml` (`vendor/` is deliberately out).
  *Gotcha*: prose that explains the ban must not trip it — the scanner strips
  `#`, and `//` + `/* */` in Rust, and conditional rules still require a
  concrete triple (`x86_64-apple-darwin`, not `*-apple-darwin`). For prose a
  comment-stripper cannot see, such as a module docstring naming `cargo xwin`,
  put `cross-lint: allow` **on that same line** — the marker is line-scoped, so
  putting it on a docstring's closing line suppresses nothing. `rg` for the
  marker lists every escape in the tree. See
  [`docs/architecture/ci.md`](docs/architecture/ci.md).

- **Adding a target** means editing `ci/ci_matrix.py` *and* adding the
  build/test job pair in `ci.yml`. They cannot be one matrix (GitHub `needs:`
  on a matrix is all-or-nothing, which would serialize every lane behind the
  slowest); `tests/test_ci_matrix.py` fails if the two drift apart.
