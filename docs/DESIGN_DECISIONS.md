# clud Design Decisions

ADR-style records for non-obvious design choices in clud. Each entry follows the structure: Context, Decision, Rationale, Alternatives Considered, Consequences.

Decisions are numbered for stable cross-references (e.g. `DD-005`). Numbers are append-only; superseded decisions stay in place with a "Superseded by" note.

---

## DD-001: Rust binary distributed as a Python wheel via maturin `bindings = "bin"`

**Context:** clud is a CLI that orchestrates other CLIs (`claude`, `codex`) on Windows, Linux, and macOS. Its distribution channel needs to reach Python developers (the primary audience already running `pip install` for AI tooling) without forcing them to install a Rust toolchain or hand-pick a binary for their platform.

**Decision:** Implement clud as a pure Rust binary in `crates/clud-bin`, then package and distribute it as a Python wheel using `maturin` with `[tool.maturin] bindings = "bin"`. Installing the wheel places the native `clud` executable onto the user's `PATH`. The Python package (`src/clud/__init__.py`) is a thin version shim with no runtime code.

**Rationale:**
- Single artifact per platform: `pip install clud` works the same on Windows, macOS, and Linux without users picking a binary.
- maturin's `bindings = "bin"` is the supported way to ship a CLI binary through PyPI; no custom wheel-building code needed.
- Rust gives us the runtime characteristics clud needs: predictable startup, no GC pauses on the PTY hot path, easy static binaries, and the `windows-rs`/`ConPTY`/COM ecosystem for Windows quirks (DD'd separately).
- PyPI also reaches the audience that runs `uv tool install` and `pipx install`, both of which extract the binary into a managed `PATH`.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Pure Python | Cannot meet startup latency goals; PTY/COM/IDropTarget work is painful or impossible in pure Python on Windows. |
| Standalone binary releases (GitHub releases only) | Users must download, chmod, and place on PATH manually. Loses the `pip install` workflow that this audience already uses. |
| Cargo install (`cargo install clud`) | Requires every user to have a working Rust toolchain. Painful on Windows. |
| Python C extension (`bindings = "pyo3"`) | Forces a Python runtime in the hot path. clud is a CLI, not a library — it doesn't need Python at all once it's on PATH. |

**Consequences:**
- The release pipeline has to build platform-specific wheels (6 platforms x 4 CI jobs = 24 jobs).
- Wheel updates trigger a hot-overwrite of `Scripts/clud.exe` on Windows, which is why `trampoline.rs` exists (rename-self-then-copy-back). See [windows-quirks.md](architecture/windows-quirks.md).
- A `clud` upgrade is `pip install -U clud` rather than a separate self-update mechanism.

---

## DD-002: YOLO mode is the default; `--safe` is the opt-out

**Context:** clud's primary value is reducing friction when running Claude Code and Codex in agent mode. The upstream agents prompt for permission on every tool call by default, which makes long-running automation impossible.

**Decision:** clud always injects `--dangerously-skip-permissions` into the backend argv unless the user explicitly passes `--safe`. This applies to every backend invocation (interactive, loop, daemon).

**Rationale:**
- Users reach for clud specifically to skip per-call prompting. Defaulting to "prompt for everything" would defeat the purpose.
- The opt-out (`--safe`) is one word, easy to remember, and preserves the safe path for users who want it.
- Single decision point in `command::build_launch_plan` means there is no path that forgets to apply the policy.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Off by default, opt-in `--yolo` | Most invocations would need `--yolo`, adding noise and creating muscle memory that defeats the safety value of the off-by-default. |
| Per-backend default (claude on, codex off) | Inconsistent UX; hard to explain. |
| Read from a config file | Adds a hidden global setting; behavior depends on machine state. |

**Consequences:**
- New users must be told about `--safe` (covered in `README.md`).
- Any code path that bypasses `build_launch_plan` would silently lose YOLO injection; see [DD-005](#dd-005-single-launchplan-as-source-of-truth-for-everything-clud-runs).

---

## DD-003: All Rust toolchain calls go through `soldr`

**Context:** clud is developed on Windows where `cargo` and `rustc` are routinely shadowed by stale shims — chocolatey's bundled cargo, rustup proxies for the wrong toolchain, system `rustc` from package managers. Builds that work locally for one developer fail for another for path-shadowing reasons that are tedious to diagnose.

**Decision:** Every `cargo`, `rustc`, and `rustfmt` invocation in this repo (developer workflow, CI, scripts) must go through `soldr <tool>` (https://github.com/zackees/soldr). soldr resolves the rustup-managed toolchain via `rustup which` and invokes that binary directly, bypassing whatever shim is on `PATH`. A `.claude/hooks/check-soldr.py` PreToolUse hook blocks any bare `cargo`/`rustc`/`rustfmt` Bash command and tells the user to install soldr.

**Rationale:**
- Eliminates "works on my machine" caused by shim drift on Windows.
- Mechanical enforcement (the hook) means new contributors hit a clear error message instead of a mysterious build break.
- soldr is a standalone binary; no Python dep, no toolchain coupling.
- CI uses `zackees/setup-soldr@v0` so local and CI invocations are identical.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Document "use rustup-managed cargo" in CLAUDE.md | Tried this; relied on each contributor reading and remembering. Drift recurred. |
| `cargo +<toolchain>` invocations | Still relies on `cargo` itself resolving to the right binary first. |
| Pin the toolchain via `rust-toolchain.toml` only | Rust-toolchain.toml works for rustup-managed cargo but not for shadowed `cargo`. Necessary but not sufficient. |

**Consequences:**
- New contributors must install soldr before they can build (`./install` or `./install --global`).
- All shell snippets in docs, `bash build`, `bash test`, etc. use `soldr cargo …` everywhere.

---

## DD-004: Backend-agnostic — support both Claude and Codex

**Context:** Users run different upstream agents (`claude` and `codex`) with similar but not identical CLI surfaces. Each backend has its own arg conventions, model-flag placement, prompt-injection mechanism, and skill-install location.

**Decision:** clud detects which backends are on `PATH` and supports either via `--claude` / `--codex` flags. The `Backend` enum is plumbed through every code path that constructs argv. Where backends diverge (`--model` placement, `-p` semantics, `stream-json` injection, the `exec`/`resume` keywords), the divergence is encoded inside `command/`. Skills bundled into the `clud` binary install to both `~/.claude/skills/` and `~/.codex/skills/` when those homes already exist.

**Rationale:**
- Locks clud into supporting users on either backend without forking the binary.
- The single-`LaunchPlan` discipline ([DD-005](#dd-005-single-launchplan-as-source-of-truth-for-everything-clud-runs)) absorbs backend divergence in one place (`command/`), so downstream code never branches on backend.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Claude-only | Cuts off users who prefer Codex or are evaluating both. |
| Two separate binaries | Code duplication; bug fixes have to land twice. |
| Adapter layer that homogenizes the backends | Premature abstraction; backend diffs are small enough to encode directly. |

**Consequences:**
- `command/` carries `if backend == Backend::Claude { … } else { … }` branches; concentrated, easy to audit.
- The skill system needs to handle two install targets, which complicates [DD-008](#dd-008-dual-skill-installer-skillsrs-vs-skill_installrs-interim-state).

---

## DD-005: Single `LaunchPlan` as source of truth for everything clud runs

**Context:** clud has many code paths that need to know "what argv will clud actually run with these flags?" — the runner itself, the daemon worker, `--dry-run` JSON output for tests, the loop iteration loop, hook health remediation, and so on. Each path independently reconstructing argv is a recipe for divergence (one path forgets YOLO injection, another places `--model` in the wrong slot).

**Decision:** Every code path goes through `command::build_launch_plan` and consumes the resulting `LaunchPlan` struct (`crates/clud-bin/src/command/types.rs`). The struct carries argv, env, prompt, optional loop markers, and optional repeat schedule. `--dry-run` serializes this struct to JSON. The runner, daemon worker, and remediator each consume the same struct.

**Rationale:**
- One implementation of "what runs" means no drift between dry-run output and actual execution.
- Tests that exercise plan construction (via `--dry-run`) automatically exercise the same path runtime uses.
- Adding a new code path that needs argv is mechanical: call `build_launch_plan`, consume the struct.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Each path builds its own argv | Verified to cause drift; YOLO and `stream-json` injection bugs found in early iterations. |
| Function-based ("`build_argv(args) -> Vec<String>`") | Loses the structured fields (prompt, markers, schedule) and forces every consumer to re-parse strings. |

**Consequences:**
- Any new launch-affecting feature must extend `LaunchPlan` rather than wire data through side channels.
- The `--dry-run` JSON contract is load-bearing for tests; breaking changes need test updates.
- See [launch-plan.md](architecture/launch-plan.md) for the construction pipeline and consumer list.

---

## DD-006: `~/.clud/data.redb` is owned exclusively by a single GC daemon process; clients access it over loopback TCP

**Context:** clud needs persistent state for tracked entries (used by `clud gc list` / `purge` / `reconcile`) and the worktree scanner. Initial implementations had every `clud` process open the redb file directly. This was unreliable under concurrent access: cross-platform advisory file locking is platform-specific, redb's own locking assumed single-process ownership, and we saw lock-contention hangs on Windows.

**Decision:** A single daemon process (`gc_daemon`) owns `~/.clud/data.redb` exclusively for its lifetime. All other `clud` processes (CLI commands, in-process worktree scanner) talk to the daemon via JSON line-delimited messages over a loopback TCP socket. The daemon serializes all redb access through a dedicated registry-worker thread. (Issue #135 Phase 1.)

The separate session-cap registry (`sessions.redb`) keeps file-lock-based serialization via a sidecar `sessions.lock` advisory lock (issue #138) because the cap-registry workload is much simpler — a per-launch row insert/remove that can tolerate brief blocking.

**Rationale:**
- One process owns the file → no cross-process locking required for the GC store.
- Loopback TCP gives us a well-understood IPC layer with no platform-specific code (Unix sockets vs named pipes).
- JSON line-delimited keeps the protocol debuggable and matches the daemon-ipc style elsewhere in clud.
- The cap-registry stays file-locked because its access pattern is rare and short; spinning a separate daemon for it would be overkill.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Continue with direct file access + advisory locks | Failed under concurrent invocations on Windows. |
| Use named pipes / Unix sockets directly | Platform-specific code; TCP loopback is portable and equally fast for this workload. |
| Move everything to a single redb file with file locks | Doesn't solve the original concurrency problem. |
| Use sqlite or a daemon-less embedded DB with better locking | redb is already used elsewhere; introducing another store fragments storage. |

**Consequences:**
- An extra process (`gc_daemon`) runs in the background; users see it in process listings.
- The daemon binary is the same `clud` executable re-entered via a hidden subcommand, so there's no separate artifact to ship.
- Connection failure to the daemon is a soft error: `clud gc list` reports unavailable, doesn't crash the user's foreground command.
- See [gc-and-registry.md](architecture/gc-and-registry.md) for the protocol.

---

## DD-007: `lib.rs` is the only place that declares modules; `main.rs` imports through `clud::{…}`

**Context:** `clud-bin` has both a binary (`main.rs`) and a library target (`lib.rs`) because Rust integration tests under `tests/` can only link against the library. If `main.rs` declares `mod session;` and `lib.rs` also declares `mod session;`, those are two separate compilation units; static state diverges, traits implemented in one aren't recognized in the other.

**Decision:** Every top-level module declaration (`mod session;`, `mod runner;`, `mod command;`, …) lives in `lib.rs` only. `main.rs` does not declare any `mod` — it imports the modules it needs via `use clud::{…}`. Integration tests in `tests/*.rs` likewise link against `clud::…`.

**Rationale:**
- Single instantiation of every module: no duplicate static state, no trait-impl mismatches between binary and tests.
- Tests can exercise internals (`session::run_raw_pty_pump`, `session::F3Observer`) by linking the library.
- Refactors that move code only need to update one declaration site.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Declare in both `main.rs` and `lib.rs` | The duplicate-instantiation problem above. |
| Declare in `main.rs` only | Tests can't import internals; would force a public-API split. |
| Make `clud-bin` a library only and have a separate `clud-cli` binary crate | More crates, slower builds, more crate-boundary friction. |

**Consequences:**
- New top-level modules require editing `lib.rs`, not `main.rs`. Easy to forget; PR review must catch.
- `main.rs` becomes a thin orchestration file rather than the project's hub. `lib.rs` is where the structural map lives.

---

## DD-008: Dual skill installer (`skills.rs` vs `skill_install.rs`) — interim state

**Context:** Skills are slash-commands (`/clud-pr`, `/clud-issue`, etc.) bundled into the `clud` binary via `include_str!` and installed into the user's backend home(s) on launch. Two installer implementations exist in the codebase today:

- `src/skills.rs` — multi-backend (`~/.claude/skills/`, `~/.codex/skills/`), non-overwriting (preserves user edits), reads from `crates/clud-bin/assets/skills/`.
- `src/skill_install.rs` — Claude-only (`~/.claude/skills/`), overwrites on semantic divergence (whitespace-tolerant compare), reads from a separate top-level `skills/` directory.

Their `BUNDLED_SKILLS` constants ship different subsets of skills.

**Decision:** Accept the duality as interim state. Both installers run on every launch. Document the divergence explicitly in [skill-system.md](architecture/skill-system.md) and the dir READMEs so contributors aren't surprised. Plan to consolidate later (single installer, single source tree).

**Rationale:**
- The two installers evolved independently — `skill_install.rs` predates `skills.rs` — and consolidating now would be a non-trivial change with its own design questions (which overwrite policy wins? which source tree?).
- Documenting the current state immediately is cheap; consolidating prematurely risks losing user edits or shipping the wrong subset.
- The non-overwriting behavior of `skills.rs` is the right policy for skills the user might edit; the overwrite behavior of `skill_install.rs` is the right policy for skills clud strictly owns. The eventual consolidation needs to preserve both modes.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Consolidate now | Requires deciding overwrite policy and source-tree layout under time pressure; risks regression. |
| Delete one installer | Either drops Codex support (`skill_install.rs` alone) or drops semantic overwrite (`skills.rs` alone). |

**Consequences:**
- Two install passes run on every `clud` launch; cost is negligible (compile-time strings, file existence checks).
- Adding a new skill requires editing `BUNDLED_SKILLS` in **both** files (and possibly placing the `SKILL.md` in both source trees). [skill-system.md](architecture/skill-system.md) documents the checklist.
- This DD should be revisited when consolidation lands; mark superseded then.

---

## DD-009: Cooperative Ctrl+C via `Arc<AtomicBool>` + best-effort descendant kill via `process_tree::kill_tree`

**Context:** clud has long-running operations (loop iterations, daemon attach, GC scan) that the user might interrupt with Ctrl+C. The interrupt needs to propagate to backend processes and clean up child processes (especially on Windows where `clud --codex` spawns `cmd.exe → node.exe`, which can orphan if the parent dies first). A tokio-style cancellation-token system would require pulling tokio into every code path.

**Decision:** Two mechanisms working together:

1. **Cooperative flag.** `startup::install_ctrlc_flag()` installs a Ctrl+C handler that sets a shared `Arc<AtomicBool>`. The flag is consumed by the iteration loop in `runner.rs`, the daemon attach loop in `daemon/attach.rs`, and the GC scanner thread in `gc.rs`. Each polling site checks the flag and exits gracefully.
2. **Best-effort descendant reap.** On exit, `process_tree::kill_tree` (via `sysinfo`) walks descendants of the current process and kills them. This fixes the multi-second Ctrl+C hang seen on Windows where `cmd.exe → node.exe` orphans the real child if only the immediate child is killed.

**Rationale:**
- The flag is dependency-free and works in sync and async code identically.
- `kill_tree` is best-effort because process trees can race (a child spawns a grandchild between enumeration and kill). Acceptable: the user's intent is "stop now"; a stray surviving process is a smaller failure mode than a several-second hang.
- Together they cover the realistic Ctrl+C scenarios without forcing every module onto tokio.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| `tokio::select!` with cancellation tokens | Forces tokio onto sync code paths; large refactor for marginal benefit. |
| Job objects (Windows) / process groups (Unix) | Platform-specific; more complex; doesn't avoid the need for an AtomicBool for sync poll sites. |
| Send SIGTERM to PID and wait | Doesn't reach grandchildren; the original codex orphan problem. |

**Consequences:**
- Every long-running loop must remember to poll the flag. If a loop forgets, Ctrl+C feels slow.
- `kill_tree` can produce stderr noise from `sysinfo` access errors on locked-down systems; suppressed where benign.

---

## DD-010: `testbins/` lives outside `crates/` for non-shipping binaries

**Context:** clud has a `mock-agent` crate that pretends to be `claude`/`codex` during integration tests. It's a Rust binary, a workspace member, and a real Cargo crate — but it's never shipped to users.

**Decision:** Test-only Rust binaries live in `testbins/` (workspace members declared in the root `Cargo.toml`), separate from `crates/` which holds the shipped binary (`clud-bin`).

**Rationale:**
- The directory name communicates intent: anything under `crates/` ships, anything under `testbins/` does not.
- Newcomers reading the repo layout immediately understand the distinction without checking each crate's `publish = false` line.
- Release tooling can mass-include `crates/*` and ignore `testbins/*` without per-crate logic.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Put `mock-agent` in `crates/mock-agent` with `publish = false` | Easy to miss the `publish = false`; mixes shipping and non-shipping crates in one directory. |
| Inline mock binary inside `clud-bin/tests/` | Cargo doesn't compile test directories as separate binaries you can find on `PATH`. The test would need to invoke the mock via library functions, which loses the integration-test value. |
| Separate test-only workspace | Two workspaces is more painful than one with a directory convention. |

**Consequences:**
- Build commands need `-p mock-agent` to target it, but that's already standard Cargo.
- Anyone adding a new test binary should put it in `testbins/`, not `crates/`. See `testbins/README.md`.
