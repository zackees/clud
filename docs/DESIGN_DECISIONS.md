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

**Decision:** clud detects which backends are on `PATH` and supports either via `--claude` / `--codex` flags. The `Backend` enum is plumbed through every code path that constructs argv and every persistent launch-setup action. Where backends diverge (`--model` placement, `-p` semantics, `stream-json` injection, the `exec`/`resume` keywords), the divergence is encoded inside `command/`. Skills bundled into the `clud` binary install to `~/.claude/skills/` for Claude Code and Codex's current `~/.agents/skills/` location only during global launch setup for the selected backend; stale clud-managed copies under the retired `~/.codex/skills/` path are purged best-effort during Codex global setup.

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

**Context:** Skills are slash-commands (`/clud-pr`, `/clud-issue`, etc.) bundled into the `clud` binary via `include_str!` and installed into the user's backend home(s) during global launch setup. Session-only launches do not write persistent skill files. Two installer implementations exist in the codebase today:

- `src/skills.rs` - multi-backend (`~/.claude/skills/`, Codex `~/.agents/skills/` gated by `~/.codex`), non-overwriting (preserves user edits), reads from `crates/clud-bin/assets/skills/`, and purges stale clud-managed Codex copies from `~/.codex/skills/`.
- `src/skill_install.rs` - Claude-only (`~/.claude/skills/`), overwrites on semantic divergence (whitespace-tolerant compare), reads from a separate top-level `skills/` directory, and purges retired managed skills from `PURGED_SKILLS`.

Their `BUNDLED_SKILLS` constants ship different subsets of skills.

**Decision:** Accept the remaining duality as interim state. Both installers remain registered behind the launch setup scope gate, and global setup runs only the selected backend's actions. Document the divergence explicitly in [skill-system.md](architecture/skill-system.md) and the dir READMEs so contributors aren't surprised. Retire merged skills through `skill_install.rs`'s `PURGED_SKILLS` list; `/clud-pr-merge` has already been folded into `/clud-pr` PR merge mode and added to that purge list. Plan to consolidate the remaining duplicate source trees later (single installer, single source tree).

**Rationale:**
- The two installers evolved independently — `skill_install.rs` predates `skills.rs` — and fully consolidating now would be a non-trivial change with its own design questions (which overwrite policy wins? which source tree?).
- Documenting the current state immediately is cheap; consolidating prematurely risks losing user edits or shipping the wrong subset.
- The non-overwriting behavior of `skills.rs` is the right policy for skills the user might edit; the overwrite behavior of `skill_install.rs` is the right policy for skills clud strictly owns. The eventual consolidation needs to preserve both modes.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Consolidate now | Requires deciding overwrite policy and source-tree layout under time pressure; risks regression. |
| Delete one installer | Either drops Codex support (`skill_install.rs` alone) or drops semantic overwrite (`skills.rs` alone). |

**Consequences:**
- Two installer implementations remain live, but they run only during selected-backend global setup. Session-only launches skip both.
- Adding a new skill may require editing one or both `BUNDLED_SKILLS` constants depending on backend coverage and drift semantics. Retiring a skill requires adding it to `PURGED_SKILLS`. [skill-system.md](architecture/skill-system.md) documents the checklist.
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

---

## DD-011: Centralized session daemon is default for interactive launches; piped invocations stay on the direct runner

**Context:** clud has two paths a user-facing session can take. The **direct runner** (`runner::run_plan_{subprocess,pty}`) spawns the backend straight from the foreground `clud` process; clean and low-overhead for a one-shot prompt. The **centralized daemon** (`daemon::run_centralized_session` → `attach_to_session`) puts a long-lived daemon between the user and the backend; gains attach/detach, kill-on-close Job Object lifetime, session listing, replay, and a uniform place to wire voice + DnD. Up through PR2 the centralized path was opt-in (`--detach`, `--experimental-daemon-centralized`, `CLUD_EXPERIMENTAL_DAEMON=1`); everything else used the direct runner.

**Decision:** Centralized is now the **default for interactive launches** — when both stdin and stdout are TTYs. Non-interactive (piped) invocations keep using the direct runner. Explicit opt-out via `--no-daemon` or `CLUD_NO_DAEMON=1`; legacy `--experimental-daemon-centralized` / `CLUD_EXPERIMENTAL_DAEMON=1` stay as forced-on aliases for back-compat.

**Rationale:**

- Every meaningful win of the centralized path (durable session, attach later, kill-on-close, session list, voice + DnD parity) only matters when there's a human at the keyboard.
- For piped one-shots the direct runner produces byte-identical stdio framing that shell pipelines and CI test harnesses depend on. Routing those through the daemon adds a TCP round-trip and an extra base64-on-pipe layer without any user-visible benefit.
- The TTY-pair check (`io::stdin().is_terminal() && io::stdout().is_terminal()`) is the cheapest, most reliable interactive-detector available and is already used elsewhere in clud (`session::terminals_are_interactive`).
- Keeps the integration test surface stable: every test that pipes its child's stdio (essentially all of `test_mock_agents.py`) stays on the direct runner without per-test annotation.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Flip the default unconditionally (centralized everywhere) | 43 integration tests broke on the trial run because they implicitly assert direct-mode behavior (stderr message wording, stdio framing). Either each test grows a `CLUD_NO_DAEMON=1` annotation or every test's expectations need updating — both invasive enough to justify the TTY-gate compromise. |
| Keep centralized opt-in indefinitely | Users with `clud foo` at the prompt should get the better experience by default; making them set an env var to opt in is friction nothing has shipped to justify. |
| Use a separate `--centralized` flag instead of repurposing `--no-daemon` | Two flags governing the same axis (`--centralized` vs `--no-daemon`) is the kind of UI papercut that compounds. `--no-daemon` already existed for the gc-daemon opt-out; extending its meaning to "skip both daemons" matches user intent: if you said no-daemon, you meant *no* daemon. |

**Consequences:**

- `clud foo` at an interactive terminal now talks to a background daemon; the daemon process becomes visible in `ps`/Task Manager. The same daemon already existed for `--detach` users — this just expands its audience.
- A first-touch `clud` may pay a one-time ~50 ms `ensure_daemon` cost while the daemon spawns. Subsequent invocations within the same session reuse the running daemon.
- `clud -p "x" | jq` and other piped uses are unchanged from the direct-runner era; no daemon involvement.
- The `experimental_enabled` function name is now misleading (centralized is no longer experimental). The function is preserved for one external call site in `main.rs` and can be renamed in a follow-up cleanup; touching its body without renaming keeps PR3's diff focused.

---

## DD-012: One always-on daemon hosts both session ops and the GC registry

**Context:** Phase 1 of issue #135 shipped a standalone `gc_daemon` process that owned `~/.clud/data.redb` and served `clud gc *` IPC ops (see [DD-006](#dd-006--cluddataredb-is-owned-exclusively-by-a-single-gc-daemon-process-clients-access-it-over-loopback-tcp)). Separately, the centralized session daemon (`daemon/`) hosted `--detach` / `attach` / `list` / `kill` / `logs` / repeat jobs but was opt-in. Two daemons per user meant two info files, two TCP ports, two lifecycles to debug, and two startup races — and the user instinct was always "there's only one clud daemon, right?"

PR #151 tried to make the session daemon the default for interactive launches but had to be reverted in PR #152 because the attach pump (`run_remote_interactive`) drops DSR/DA/OSC replies via `crossterm::event`. With the centralized-by-default plan off the table, the always-on slot was empty.

**Decision:** Merge `gc_daemon` into the session daemon. There is now exactly one `clud` daemon process per user, auto-spawned from `main.rs` on every non-`--no-daemon` / non-`--dry-run` invocation. It serves the existing `Create` / `Session` / `Terminate` ops plus a new `Gc { payload }` variant that routes to a registry-worker thread inside the same process. Foreground interactive launches still use the direct runner (until the attach pump is rewritten); the daemon hosts the centralized PTY path only when explicitly opted in (`--detach`, `--detachable`, `--experimental-daemon-centralized`, repeat jobs).

This supersedes the "separate GC daemon" half of [DD-006](#dd-006--cluddataredb-is-owned-exclusively-by-a-single-gc-daemon-process-clients-access-it-over-loopback-tcp) — the single-owner-of-redb invariant survives, only the owning process identity changed. The `gc_daemon.rs` module and `__gc-daemon` hidden subcommand are gone.

**Rationale:**

- One process per user matches the user's mental model and halves the surface area for "is the daemon up?" diagnostics.
- redb's single-process-ownership invariant is preserved: the registry worker thread is still the sole reader/writer of the file.
- The session daemon's existing infrastructure (`ensure_daemon`, `trampoline::spawn_detached_self`, info file, stale-state cleanup) covers everything the standalone GC daemon needed.
- Auto-spawning the session daemon unconditionally (not just when GC is touched) means later phases of #135 (background reapers, graveyard) have a host process that's already running and warm.
- Avoids spawning two separate detached children from the same parent, which previously destabilized the freshly-spawned session worker on Linux (per the deleted "skip when `experimental_enabled`" comment in `main.rs`).

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Keep both daemons | The maintenance and UX cost (two info files, two ports, two race windows, two readme entries) compounds with every reaper/graveyard feature added to either. |
| Merge under `gc_daemon` instead of under `daemon/` | The session daemon has the richer feature set (PTY worker subprocesses, attach pump, snapshot/log persistence) and a stable IPC enum protocol; lifting GC into it is a smaller diff than lifting session-management into `gc_daemon`. |
| Run GC inside the session daemon only when `experimental_enabled` is true | Keeps GC unavailable in the common case (foreground direct-runner launches). Defeats the always-on goal. |
| Add a `--daemon=gc` / `--daemon=session` mode flag and keep two binaries | The mode flag was the design in #135 §1 but added complexity (one binary, two long-lived state directories) for no end-user benefit. |

**Consequences:**

- Daemon state dir is now `~/.clud/state/` (persistent) instead of `$TMP/clud-daemon` (transient). Survives reboots; aligns with the GC daemon's prior location so the redb file stays put.
- `clud --no-daemon` and `CLUD_NO_DAEMON=1` now skip both spawn and registry access. `clud gc *` with `--no-daemon` is an error (no read-only fallback, unchanged from prior).
- One-time migration: users with a running pre-merge `gc_daemon` process will hit a redb lock conflict on first post-merge run; the old process idle-shuts after its 30-min window or can be killed manually. The redb file itself is forward-compatible.
- DD-006's "single owner" promise is intact; only the process identity moved. DD-011's "centralized as interactive default" remains reverted (per PR #152) and is independent of this change.

---

## DD-013: rusqlite and redb coexist in clud-bin

**Context:** The agent-memory subsystem (META #255) needs persistent storage with two access patterns: SQL writes against a typed schema (memories, sessions, relations, lessons, actions) and KNN over dense embeddings. The natural fit for both — and the only one with a mature loadable vector extension — is SQLite via `rusqlite` plus `sqlite-vec`. But clud already runs `redb` (DD-006, DD-012) as the always-on daemon's single-owner store for the gc/session registry, and `rusqlite` was deliberately removed from the dep graph when redb was adopted (see the comment at `crates/clud-bin/Cargo.toml:31`) to cut cold-build time and drop C-toolchain pressure on CI.

**Decision:** Re-add `rusqlite = { version = "0.31", features = ["bundled", "load_extension"] }` for the memory subsystem only. The two stores coexist in the same binary with disjoint files and disjoint ownership: redb continues to own `~/.clud/state/data.redb` for the gc/session registry; rusqlite owns `~/.clud/state/memory/memory.db` for the memory store. Neither store reads or writes the other's file.

**Rationale:**
- `sqlite-vec` is the only loadable vector extension that runs inside SQLite's transaction boundary on the same connection that handles SQL writes. Keeping `memories` and `memory_vec` in one `BEGIN IMMEDIATE` is the load-bearing invariant for the storage layer (kill mid-tx must not leak a half-row).
- Picking redb for the memory store would mean either hand-rolling KNN (no embedded vector backend in the redb ecosystem) or running two stores anyway with a foreign-key shim between them. The cost of two stores is real but bounded; the cost of a hand-rolled vector index is unbounded.
- Migrating the gc/session registry back to SQLite would be a destructive change to a stable subsystem (DD-006, DD-012) for no net win — redb's single-process-ownership story works for that workload.
- `rusqlite bundled` adds a `cc` build step that we accept; CI was already paying for `whisper-rs-sys`, `sqlite-vec`, `whisper-rs`, and `tantivy`'s pure-Rust compile, so adding `libsqlite3-sys` is incremental.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| One store (redb only), hand-roll KNN | Requires an embedded ANN index from scratch (hnsw / IVF in pure Rust against redb tables); large new surface area; no battle-tested option. |
| One store (SQLite only), migrate gc registry off redb | Destructive migration on a shipped subsystem; redb's single-process invariant is exactly what gc wants; SQLite would just re-introduce the cross-process locking story we left behind in DD-006. |
| `redb` for SQL + `qdrant`/`lance`/external for vec | Spawns or links a heavier-weight system; runs counter to the always-on-single-binary posture; daemon process model would have to host or proxy another service. |
| Defer the entire memory subsystem | Blocks META #255 indefinitely; the architectural fit for the memory store is independent of gc/session storage. |

**Consequences:**
- Two embedded DBs in the same process: contributors must know which file owns which fact. The directory structure makes this visible (`~/.clud/state/data.redb` vs `~/.clud/state/memory/memory.db`).
- Cold builds get the `cc` step for `libsqlite3-sys` + `sqlite-vec` back. Measured cost on Windows x64: ~25 s added to a clean compile, already amortized after one `soldr cargo` cache hit.
- The `bundled` feature ships a vendored SQLite copy with every wheel, sidestepping system-SQLite version drift but inflating the binary by ~1.2 MB.
- Windows-ARM may not build `sqlite-vec`; the memory module reserves a `whisper-rs`-style target-cfg carve-out (PR1 ships without it; CI on `windows-11-arm` is the decider).

---

## DD-014: Repo URL as primary memory scope; branch as metadata, not partition

**Context:** Agent memory needs to answer "is this fact about *this* project?" without leaking facts across unrelated clones, and without locking memories away on a single branch. Common cases: (a) the user records "auth uses HS256 from vault" on `feature/oauth`, then merges to `main`, and expects the memory to still apply on `main`; (b) the same user runs `clud` in two checkouts of the same clone (worktree, or a separate `git clone`) and expects the same memory bucket; (c) a fork (`other/clud` vs `zackees/clud`) is a *different* project until explicitly bridged.

**Decision:** The agent-memory scope partition key is the **normalized `origin` URL** (`repo://<host>/<owner>/<repo>` after `normalize_origin_url`), with a `dir://<canonical-common-dir>` fallback for repos with no remote. Branch is recorded as `branch_name` provenance metadata but is **not** part of the partition key by default. Worktrees of one clone resolve to the same key automatically via `git rev-parse --git-common-dir`. The user opts out per-branch by creating `<common_dir>/.clud/memory-branch-isolate`, which appends `#branch=<branch>` to the scope key for that working tree.

**Rationale:**
- Cross-branch continuity is the common case for project facts: build commands, library choices, conventions. Partitioning by branch would silently hide those facts whenever the user switches branches, defeating the purpose of long-lived memory.
- Cross-repo isolation (forks, unrelated projects) is the *other* common case. Origin URL is the only stable identifier that distinguishes a fork from its upstream without false negatives (paths differ between machines; remote names differ between users).
- Worktree convergence falls out for free: `--git-common-dir` already points at the primary's `.git` from any linked worktree, so one normalize-step gives identical keys.
- Orphan branches are still parented to the clone's `origin` — the orphan's contents are likely still about the same project, and the user can opt out per-branch if not.
- The opt-out is a single marker file (not a config-key, not a sub-database), so it travels with the repo if the user commits it, and is a one-line decision to make or revoke.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Branch as part of the partition key by default | Switching branches hides project facts. Promotion logic would need a "branch-agnostic" tier anyway, recreating today's behavior with extra steps. |
| Path-based key only (`canonical-working-dir`) | Different machines have different paths; the same clone moves between checkouts; loses cross-machine continuity that the origin URL gives for free. |
| One memory bucket per machine, branch-agnostic | Cross-repo pollution: memories from `~/other/foo` would leak into `~/this/bar` whenever the user runs `clud` in either. |
| Per-branch scope unless user opts *in* to sharing | Inverts the common case — most facts are cross-branch project facts, not branch facts. The bias should make the common case free. |
| Treat orphan branches as their own scope by default | The orphan's contents are still about the same project most of the time (try/throwaway branches, doc-only branches). Forced isolation costs the user expected continuity; the opt-out marker is the right granularity. |

**Consequences:**
- A user who genuinely wants per-branch isolation (e.g., spike branches that contradict main's conventions) has to run `clud memory branch-isolate` once per branch.
- Renaming a remote (e.g., `zackees/clud-old` → `zackees/clud`) produces a new partition. We accept the user-visible rename as the trigger for a deliberate `clud memory rekey` (deferred to v0.5; out of scope for #267).
- Forks are isolated by default — exactly what the user wants. Bridging two forks requires the explicit `--repo-glob` flag at query time (consumer wiring in #259 / #262).
- The marker file lives at `<common_dir>/.clud/memory-branch-isolate` instead of inside the SQLite store. That's deliberate: the decision should travel with the working tree via `git`, not be hidden in a per-user database.

---

## DD-015: Local embedder via fastembed + Windows-ARM carve-out

**Context:** The agent-memory subsystem (META #255) needs to turn arbitrary text into a dense vector before it can be stored in the `memory_vec` `vec0` virtual table. The choice is between (a) calling a remote embedding API on every save (Anthropic/OpenAI/Gemini), (b) running a local model in-process, or (c) shelling out to an external embedder service. The default-experience target — `clud memory_save` after `claude` ships a long-form lesson — is latency-sensitive (sub-second), privacy-sensitive (lessons may contain proprietary code), and offline-capable (a clud user on a flight should not lose memory).

**Decision:** Default to a local in-process embedder via `fastembed = "4"` (which pulls `ort = "2.0.0-rc.x"` ONNX Runtime). Model: `AllMiniLML6V2` (384-dim, ~80 MB on disk, ~30 ms / embed on CPU). The dep is feature-gated behind `memory_local_embed` (default-on) and target-gated to `cfg(not(all(target_arch = "aarch64", target_os = "windows")))` because `ort` does not yet ship a prebuilt `aarch64-pc-windows-msvc` ONNX Runtime. The carve-out mirrors the existing `whisper-rs` stanza at `crates/clud-bin/Cargo.toml:103`. On Windows-ARM, `Embedder::Local` is not a variant of the enum (compile-time absent, not a stub); `embedder_from_env` falls through to `Embedder::Remote` (if `CLUD_MEMORY_EMBEDDER_PROVIDER` is set) or `Embedder::Disabled` (with a verbose four-path remediation message).

**Rationale:**
- In-process ONNX is 10–100× cheaper than the round-trip to a remote API and runs offline. For a feature whose UX is "type once, save once, search later", any user-visible latency on save is felt directly.
- `fastembed`'s default cache lives under a state dir we control (`<state_dir>/memory/models/`), so the model is downloaded once and reused — matches the existing `whisper-rs` cache pattern.
- Remote providers remain a first-class fallback for users who can't or won't run a local model (corp policy, Windows-ARM, embedded systems). Four providers cover the four common buying patterns: existing Anthropic key, existing OpenAI key, existing Google key, self-hosted Ollama on LAN.
- Compile-time variant absence on Windows-ARM (versus a `Local(())` stub returning `Err`) forces every caller to handle the case at the type level — the same trick `voice/worker.rs:18-21` uses for `WhisperContextHandle`, but stricter: a misuse is a build error, not a silent no-op at runtime.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Remote-only (no local at all) | Latency, privacy, offline. Defeats the "claude saves a lesson and it's instantly searchable" UX. |
| Local-only (no remote fallback) | Strands Windows-ARM users and corp-firewalled installs; ort prebuild story is too immature to bet on. |
| Pre-bundle the MiniLM .onnx in the wheel | +80 MB wheel size on every platform; download-on-first-use is fine for a daemon that has hours/days of runtime. |
| `Local(())` stub returning `Err` on Windows-ARM | Defers the carve-out to runtime; loses the type-level guarantee that no caller forgets to handle the absent backend. |
| Re-use the whisper-rs ONNX runtime (it links one already) | whisper-rs uses GGML, not ONNX; sharing ort would require a different whisper backend entirely (unrelated PR). |

**Consequences:**
- Adding `fastembed/ort` pulls a sizeable C++ link step on 5 of 6 platforms; CI cold-build time goes up by ~1–2 min, amortized after one cache hit. zccache via soldr handles incremental compiles.
- Windows-ARM CI runs `cargo build --no-default-features` to verify the carve-out continues to compile cleanly. Users on Windows-ARM see the four-path message until they set `CLUD_MEMORY_EMBEDDER_PROVIDER` or switch to WSL2.
- `ort 2.0.0-rc.x` is pre-release; fastembed v4 is the integration point. Risk: rc churn. Mitigation: if rc.x breaks, pin fastembed v3 + ort 1.16; the `Embedder` trait is stable across either backend.
- `clud memory reembed --model <new>` (CLI verb lands in #262) will use this PR's `reembed_all` library primitive to swap embedders end-to-end, including across dim changes (Local 384 → Ollama 768).

---

## DD-016: Three-tier auto-forget is scoped to Working only

**Context:** The agent-memory subsystem (META #255) models retention with three tiers: Working (per-session scratch), Episodic (session-summarized), and Semantic (durable cross-session knowledge). Auto-forget — the daemon-side TTL sweep — needs a clear contract about which tiers it may delete. The natural temptation is to let TTL apply to every tier with progressively longer thresholds, scoring each row against recency + access + score thresholds and dropping the lowest-ranked rows in any tier.

**Decision:** `memory::tiers::forget_expired` deletes **only** Working-tier rows whose `now_ms - last_access_at_ms > working_ttl_ms`. Episodic and Semantic rows are never auto-forgotten. Users opt into longer-term storage via promotion (Working → Episodic → Semantic), which is itself gated by access-count and dwell-time floors. Removal of Episodic/Semantic is an explicit MCP delete (sibling sub-issue) or — eventually — a user-confirmed forget-candidate flow surfaced from `retention_score`. The score function exists and is used for surface ranking, but the TTL sweep ignores it for Episodic/Semantic.

**Rationale:**
- Predictable retention is the property users actually want from a long-lived memory store. "I promoted this into Semantic" should mean "this lives until I delete it," not "this lives until the decay model says so."
- The promotion gate (access-count floor + dwell time) already makes Episodic/Semantic placement intentional. Adding a second gate where the daemon can override that intent silently is poor UX — users would lose memories they thought they had locked in.
- Working has natural lifecycle bounds (a session ends, scratch is no longer relevant); a TTL sweep matches that mental model.
- Auto-forgetting Episodic/Semantic is the kind of failure that's hardest to recover from: deleted rows have no audit trail and no user-visible warning before they go. The cost of leaving stale Semantic rows around is bounded; the cost of deleting a real memory is unbounded.
- `retention_score` still exists, returns a useful number, and can drive a *review*-style flow later. We just don't connect it to the delete path.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Three-tier TTL (each tier gets its own threshold) | Hides retention behind two policies (promotion + decay); users can't predict what survives. Surveyed in the sibling issue #258 spec under "auto-forget pass" — explicitly rejected for v1. |
| Score-driven forget across all tiers | Combines decay model + access boost + tier floor into one number; great for ranking, terrible for deletion because the user can't intuit which side of the threshold a row will land on. |
| Soft-delete with trash-table for all tiers | Adds storage cost and a second surface for "where did my memory go?" diagnostics. Working has no value as a tombstone — it's scratch. |
| Never auto-forget any tier | Working bloats indefinitely with per-session noise; embedding cost grows without bound. |

**Consequences:**
- The `TierConfig::working_ttl_ms` knob is load-bearing; the equivalents for Episodic/Semantic don't exist and shouldn't be added without revisiting this DD.
- Users who outgrow Working scratch must manually delete (via the eventual MCP delete verb) or wait for the TTL. There is no "decay-driven forget" escape hatch in v1.
- `retention_score` returning a low value is informative, not actionable on the storage path — sibling sub-issues may build surfacing UIs from it (forget-candidate review).
- This DD is at the policy layer, not the API layer: `SqliteStore::delete` still accepts any id. The tier-gated rule lives inside `tiers::forget_expired` and is the only daemon-driven caller.

---

## DD-017: Memory service runs in-process inside the existing clud daemon

**Context:** The agent-memory subsystem (META #255) needs a process to own four resources: the SQLite store, the tantivy `IndexWriter`, the embedder, and the consolidation timer. The natural choice is between a sidecar `clud-memory` process (mirroring how Phase 1 of #135 ran the standalone `gc_daemon`) and folding it into the existing `clud` daemon next to the GC registry worker and the dashboard's `tiny_http` listener.

**Decision:** Spawn the memory subsystem in-process inside `clud`'s session daemon. `daemon::memory_service::spawn_memory_service(state_dir)` runs alongside `gc_service::spawn_registry_worker_for_state` and `http::spawn_dashboard` from `daemon::server::run_daemon`, before the daemon's IPC accept loop. The result is held as `Option<Arc<MemoryService>>` and shared with the dashboard's HTTP handlers (and, in future PRs, the MCP server and the hook subcommands). Each resource is held inside the service: `Arc<Mutex<SqliteStore>>`, `Arc<Mutex<LexicalIndex>>`, `Arc<Embedder>`, and `TierConfig`.

**Rationale:**
- One daemon per user is already the established pattern (issue #135 merged the standalone `gc_daemon` into the session daemon for exactly this reason). Adding a second sidecar would re-introduce the multi-process orchestration problem #135 closed: bringup ordering, lockfile coordination, version skew across upgrades, and stale-pid cleanup.
- The dashboard's HTTP server already runs in-process and already needs every resource the memory service owns. A sidecar would have to expose its own IPC for the dashboard to call back into.
- The embedder load cost (fastembed model download + ONNX session init on first run, plus a few hundred ms warm) is one-shot per daemon lifetime. Doing it once at daemon start is cheaper than per-request and avoids the cold-start tax every hook subcommand or MCP request would otherwise pay.
- `Arc<Embedder>` is cheap to share. The embedder type is `Send + Sync` with no internal mutex; the timer thread, every HTTP handler, and every future MCP request all read from the same `Arc` clone.
- The consolidation timer is a thin loop that takes the SQLite + lexical mutexes for the duration of one round of `promote_candidates → apply_promotions → forget_expired`. Cross-process orchestration of that loop from a sidecar would be strictly worse — more code, more failure modes, no upside.
- Failure of any single piece must not take the daemon down: a bad embedder env falls back to `Embedder::Disabled`; an unopenable SQLite leaves `memory_service: None`; HTTP routes return 503. The session daemon's other duties keep running.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Sidecar `clud-memory` process | Re-introduces the multi-process bringup problem #135 closed. The dashboard would need a second IPC path to call into memory; tests would need to spawn and reap a second binary. No upside that justifies the cost. |
| Memory subsystem lazily loaded on first MCP request | Pushes the embedder cold-start onto the user's first save / search; defeats the "instantly searchable" UX the embedder choice was made to protect (see DD-015). The 100% case is "memory is enabled," so eager load wins on the common path. |
| Memory subsystem inside the per-session worker process | Each session would have to load its own embedder and open its own SQLite handle — embeddings would be N× the disk + RAM cost, and SQLite writes from N workers would need cross-process locking. Defeats the single-writer invariant the storage layer relies on (memory.md). |
| Tokio runtime for the timer + handlers | The daemon already uses `std::thread` everywhere (GC, dashboard); adding a runtime just to drive a 5-minute timer is the wrong tool. The future MCP server (sub-issue #259) may pull tokio in, but it can contain it inside its own thread; this PR doesn't preempt that choice. |

**Consequences:**
- Daemon startup runs the embedder load — first run pays the model download (~80 MB MiniLM ONNX); subsequent runs hit the on-disk cache. Logged but not fatal.
- `DaemonInfo.memory_mcp_port` is reserved as `Option<u16>` so issue #259 can populate it without a wire-format break.
- The consolidation timer holds the SQLite + lexical locks for the duration of one tick. Saves and searches are blocked for the tick's duration — typically tens of ms on a small store, bounded by `O(N)` in the worst case where the entire Working tier is being promoted in one round. Acceptable for v1; if it bites, future work can split the lock or batch the apply.
- The reconciliation pass on startup is `O(N)` upserts plus one tantivy commit; bounded by row count and cheap on a clean daemon. Documents the cost in `crates/clud-bin/src/daemon/memory_service.rs` so future readers know the bound.
- The HTTP `/memory/*` route bodies are stubs in this PR (#261); the real implementations land in #263. The seam is `MemoryService` references in each handler — no new IPC needs to be added when #263 lands.

---

## DD-018: MCP server embedded in clud daemon vs sidecar binary

**Context:** Issue #259 needed to expose the agent-memory subsystem to MCP clients (Claude Code, Codex). The choice was between (a) shipping a separate `clud-memory-mcp` sidecar binary that MCP hosts spawn directly and which opens its own SQLite/tantivy handles, and (b) folding the MCP server into the existing `clud` daemon next to the GC registry, the dashboard, and the memory consolidation timer. The MCP client integration is a thin `clud mcp` stdio bridge in either case; what differs is who owns the storage handles.

**Decision:** Embed the MCP server in the clud daemon. `daemon::memory_mcp::spawn_mcp_server(memory: Arc<MemoryService>)` runs alongside `spawn_dashboard` and the consolidation timer from `daemon::server::run_daemon`, binds an ephemeral loopback TCP port, and writes that port into `DaemonInfo.memory_mcp_port`. The `clud mcp` subcommand is a thin stdio↔TCP bridge that calls `daemon::ensure_daemon` and proxies bytes to the loopback port.

**Rationale:**
- Single source of truth for `MemoryService`. The dashboard, the consolidation timer, the hook subcommands (#260), and the MCP server all share one `Arc<MemoryService>`. A sidecar would have to re-open SQLite (single-writer constraint! cross-process locking!), re-open tantivy (per-directory `LockBusy`), and re-load the embedder (the ~80 MB ONNX session at first run). Net: more code, slower cold-start, and a real concurrency problem to solve, with no upside.
- Lifecycle is already solved. The daemon's `ensure_daemon` + bringup lock + version skew detection (DD-012) already handles "is there one of these per user, and is it the current version?". A sidecar would re-do all of that.
- IPC overhead is already paid. The dashboard's `/memory/*` routes go through the same `MemoryService` Arc; a sidecar would need a second IPC path to call back into the dashboard's data, or the dashboard would have to learn to talk to the sidecar via TCP, doubling the surface area.
- The `clud mcp` bridge is pure `std::net` + two `std::thread`s. No tokio runtime, no rmcp dependency churn. Adding 200 LOC of std-only bridge code is strictly cheaper than introducing async to the daemon process.
- Failure isolation is preserved. If the memory subsystem fails to start, `memory_service: None`, and `memory_mcp_port` stays `None`. The bridge emits a clear JSON-RPC error (`-32099`) and exits 1 rather than hanging. Sessions, GC, and the dashboard keep running.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Sidecar `clud-memory-mcp` binary | Cross-process SQLite handle ownership is the actual blocker — sqlite-vec's single-writer rule means only one process can have the write handle. We'd either need IPC from the sidecar to the daemon for every write (defeating the sidecar) or move the daemon's writes into the sidecar (turning the daemon into the sidecar's IPC client). Lose-lose. |
| In-process but on a tokio runtime via `rmcp` | rmcp 1.7 pulls in tokio + tokio-util + schemars + (transitively) hyper-bits. The MCP wire protocol is line-delimited JSON-RPC 2.0; it doesn't need any of that. A 350-line hand-rolled NDJSON dispatcher on `std::net::TcpListener` covers initialize / tools/list / tools/call. Saves ~50s of cold-build time × 6 CI targets and keeps the daemon's "no async runtime" property (DD-017). |
| Defer MCP entirely; only ship CLI verbs | Forces every prompt-time memory access through `clud memory search` subprocess hops. MCP is what Claude Code and Codex already speak; missing it is a v0.1 regression. |
| Streamable HTTP transport instead of TCP | MCP supports HTTP + SSE, useful for team-shared memory hosts later. Loopback TCP is simpler, has no auth surface, and matches the daemon's existing pattern. Streamable HTTP can be added in v0.5 without breaking the TCP transport. |

**Consequences:**
- Adding new MCP tools is a one-place edit in `daemon::memory_mcp` — no IPC schema to extend, no sidecar binary to rev.
- The `clud mcp` bridge depends on `daemon::ensure_daemon` succeeding. A daemon-down case shows up as a stdout JSON-RPC error to the MCP host within ~5s (the `ensure_daemon` timeout); never silently hangs.
- We carry the JSON-RPC wire protocol ourselves. Cheap (single file, fully unit-tested) but it does mean tracking the MCP protocol version manually. The advertised version (`2024-11-05`) is the one Claude Code's MCP host currently negotiates.
- Adopting `rmcp` later is non-breaking — the file is self-contained and the wire shape (`{ content: [{type: "text", text: "<json>"}] }`) is exactly what `rmcp` emits. A future migration is a behind-the-curtain rewrite, not a public-API change.
- `memory_reflect` ships as a documented stub returning a `-32603` internal error with a `v0.5` note. The reflect tool depends on knowledge graph (deferred past v1 per META #255) and an LLM provider (#257 v0.5 ladder), neither of which is on main yet. The stub means MCP hosts get a clean error today instead of a missing-tool error.

---

## DD-019: `clud memory` CLI verbs proxy mutating ops through the daemon

**Context:** The `clud memory` CLI surface (issue #262) needs to talk to the SQLite + tantivy + embedder stack that lives inside the always-on `clud` daemon ([DD-017](#dd-017-memory-service-runs-in-process-inside-the-existing-clud-daemon)). The CLI is a separate short-lived process; the daemon is the long-lived process that holds the `Arc<Mutex<SqliteStore>>` and `Arc<Mutex<LexicalIndex>>`. There are two ways the CLI can perform a save / forget / reembed: (A) open the on-disk files itself and write through, or (B) round-trip the request through the daemon's existing `tiny_http` loopback listener.

**Decision:** Every mutating `clud memory` verb (and the read-only `status` / `search` / `recent` verbs) talks to the daemon via the dashboard's `/memory/*` HTTP routes. `branch-isolate` / `branch-unisolate` are the only verbs that bypass the daemon — they write a marker file under the working tree's git common-dir and never touch the SQLite or tantivy state. The `--to-disk` / `--from-disk` flags on `export` / `import` are stubs that defer to #264.

**Rationale:**
- **Single SQLite writer per process.** rusqlite holds a process-global lock per opened connection. If the CLI also opened `memory.db` while the daemon held it, the second open would either fail outright (WAL with `EXCLUSIVE` mode) or succeed and race with the daemon's `BEGIN IMMEDIATE` calls. Daemons are notoriously bad at recovering from the resulting `SQLITE_BUSY` / `SQLITE_LOCKED` errors when the conflicting writer is in another process. Routing through the daemon collapses both writes onto the daemon's single mutex.
- **tantivy `IndexWriter` is per-directory exclusive.** Two `IndexWriter`s against the same directory return `LockBusy`. Same failure mode as SQLite, same fix.
- **Embedder is already loaded in the daemon.** The CLI saving a row has to embed it. The daemon already paid the embedder cold-start (#257); making the CLI load its own embedder would double the cost on every save (and would race against the daemon's local model cache directory on Windows).
- **HTTP routes were 95% wired up by #261.** The `tiny_http` server already had the routes registered as stubs. Replacing the stub bodies with real implementations is a smaller change than introducing a new IPC channel just for memory ops.
- **Read-only ops go through HTTP too for consistency.** A `clud memory status` that opened the SQLite file read-only would work, but it's one more code path to maintain and one more way the on-disk schema version can drift from what the daemon thinks. HTTP keeps `status` aligned with whatever the daemon is currently serving.
- **`branch-isolate` is a marker-file write only.** No SQLite, no tantivy. Forcing it through the daemon would mean another route and a daemon round-trip for a `fs::write` call. The marker semantics are entirely local to the git common-dir.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| CLI opens SQLite + tantivy directly | Races against the daemon's writer; two `IndexWriter`s on tantivy hit `LockBusy`; two `BEGIN IMMEDIATE` SQL writers hit `SQLITE_BUSY`. The user's failure mode is "save succeeds half the time." |
| CLI uses the daemon's existing JSON-line IPC (the GC channel) | The IPC channel is single-threaded and already serializes session + GC ops. Adding memory ops would stall on the GC worker thread. HTTP has its own threadpool inside `tiny_http`. |
| CLI talks to an MCP server | The MCP server (#259) doesn't exist yet at the time #262 ships. Building #262 on top of #259 would reverse the dependency order and block #262 on the MCP wire-format design. |
| Make every CLI verb an in-process `--daemon-state-dir` sub-mode (`__memory`-style hidden command) that the daemon shells out to | Doubles startup latency on every verb (Rust binary startup + linker time) and the in-process variant of `MemoryService` is what we built in #261 already — HTTP is the documented seam. |

**Consequences:**
- The CLI is now a thin shell around HTTP requests. Each verb is ~50 LOC of validation + pretty-printing.
- `daemon::http_save` / `http_forget` / `http_search` / `http_stats` / `http_recent` are the public seam (`crates/clud-bin/src/daemon/memory_client.rs`). Future sub-issues (MCP, hooks) can call them too, or use the in-process `MemoryService` Arc directly when running inside the daemon process.
- The dashboard's `/memory/*` route bodies are no longer stubs — they delegate to the live `MemoryService` they already received. #261's `MemoryService` parameter is now load-bearing.
- `--no-daemon memory <verb>` exits 3 with a clear message; there is no "offline" mode for the memory CLI. The two verbs that don't need the daemon (`branch-isolate`, `branch-unisolate`) handle `--no-daemon` implicitly by never calling `ensure_daemon`.
- Live `clud memory reembed` against a running daemon is intentionally not implemented in v1 — the route would need to hold the SQLite mutex for the duration of an O(N) walk and would starve other ops. The CLI's `reembed --dry-run` reports counts, and the user is pointed at stopping the daemon for the real rewrite. A dedicated `POST /memory/reembed` with batching is future work.

---

## DD-020: Memory dashboard tab stays in the vanilla-JS SPA pattern

**Context:** Issue #263 needs to add a "Memory" tab to the daemon dashboard so users can browse stats, recent rows, search hits, and a forget action without dropping into the CLI. The existing four tabs (Sessions / Garbage / Repos / Ctrl-C) live in a single `index.html` file under `crates/clud-bin/assets/dashboard/`, served via `include_str!` from `daemon::http`. There is no build step, no `npm`, no bundler — the file is loaded by the browser exactly as it sits on disk.

A new tab is the second-largest UI change the dashboard has ever taken (after the original tab system itself). The temptation to "do it right" with React + a build step is real: the Memory tab has five cards, a poller, a search input, a forget confirmation, and a save form. That's the threshold at which most teams reach for a framework.

**Decision:** The Memory tab is plain vanilla JS in the existing `index.html`. No React, no Vue, no Lit, no bundler, no TypeScript. The tab is appended in-place to the existing single-file dashboard, reusing the same CSS variables, the same `esc()` helper, the same `fetch()` calls, and the same 5s polling loop as the four existing tabs.

**Rationale:**

- **No version skew between the Rust binary and the bundled JS.** The HTML ships inside `include_str!` so the asset travels in lockstep with the binary. A bundler would introduce a build artifact that has to be regenerated whenever the JS changes; forgetting to rebuild produces a binary that disagrees with the source tree, and the failure mode is silent — the page just renders stale UI.
- **No npm in the Rust build.** The CI matrix is 24 jobs across 6 platforms. Adding a Node toolchain to those jobs doubles the cold-start cost on every PR for a tab that is fundamentally five fetch calls and three `innerHTML` writes.
- **The existing tabs are the style guide.** Sessions / Garbage / Repos / Ctrl-C are all renderer functions that take a `state` blob and write to a DOM container. The Memory tab follows the same shape: `pollMemoryStats()`, `pollMemoryRecent()`, `renderMemoryStats()`, `renderMemoryRecent()`. Anyone who can read the existing four tabs can read the fifth one.
- **File size is still tractable.** Post-#263 the dashboard is ~870 LOC including CSS, well under the 1k threshold past which agents start to thrash on file reads. If the tab grows another 500 LOC the per-tab split becomes worth the cost; today it isn't.
- **Auto-refresh folds into the existing 5s tick.** The four other tabs already poll `/state.json` every 5s; the Memory tab polls `/memory/stats` and `/memory/recent` on the *same* tick, gated by "is the Memory tab the active section?" so the SQLite mutex stays cold for users on other tabs.
- **Anchor routing comes for free.** Six lines of JS map `#memory` → activate the Memory tab on load and on `hashchange`. The CLI verb `clud memory ui` (issue #262) opens `http://127.0.0.1:<port>/#memory` so users land on the tab directly. No router library needed.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| React + Vite build step bundled into the binary | Adds a Node toolchain to all 24 CI jobs; introduces a build artifact (`dist/index.html`) that has to stay in sync with the source tree; the version-skew failure mode is silent. |
| Lit / Vue / Svelte single-file component | Same toolchain cost; no obvious win over vanilla JS for a tab whose state model is "five fetch calls and three renderers". |
| Move the dashboard out of the binary entirely (serve from disk) | Breaks the single-binary distribution model. Users would have to install the dashboard separately, or the binary would need a "first-run extract" pass that nobody wants to debug on Windows. |
| Make the Memory tab a separate `index-memory.html` page | Doubles the polling traffic (two `/state.json` consumers), forces users to switch URLs instead of tabs, and makes deep-linking (`#memory`) impossible. |

**Consequences:**

- The dashboard file grows from ~416 to ~870 LOC. Still trivially `include_str!`-able. The next tab can fit in the same file; the tab after that probably warrants a `assets/dashboard/memory.js` split, served as a separate route.
- The dashboard's CSS surface area grows with the new tier-badge classes (`tier-working` / `tier-episodic` / `tier-semantic`) and the embedder-status pill (`embedder-ready` / `embedder-warn`). All new classes reuse the existing `--accent` / `--live` / `--warn` palette tokens; no new tokens were added.
- The Memory tab handles "daemon unreachable" via the same `try { ... } catch { ... }` shape used by `refresh()` — the error message is rendered in a banner above the stats card with a retry button. No special offline mode; the page just shows the error and the next poll either recovers or paints the error again.
- Two new tests pin the contract: a Rust unit test asserts the bundled HTML carries the tab + endpoint references, and a Python integration test boots the daemon and asserts `/memory/stats` returns the field shape the JS reads. The JS itself is not unit-tested; the Playwright suite (`tests/integration/test_ui_dashboard_playwright.py`) is the appropriate place for that, future-tense, once it grows a Memory-tab section.

---

## DD-021: Git-artifact format — per-file Markdown with YAML frontmatter; Semantic-only by default

**Context:** Agent memory in the SQLite store ([DD-013](#dd-013-rusqlite-and-redb-coexist-in-clud-bin)) is durable but not portable: there is no way for a teammate to inherit "the project knows `auth uses HS256 JWTs from vault`" short of copying `~/.clud/state/memory/memory.db`. Issue #264 (sub-issue 9 of META #255) needs a serialization format that (a) survives `git pull` cleanly, (b) is human-reviewable so users can see what is about to be committed, (c) defaults conservatively on privacy, and (d) supports tier-gated visibility so transient Working-tier rows never leak into the repo.

**Decision:** Serialize each exportable memory as a standalone Markdown file under `<git-root>/.clud/memory/<tier>/<ULID>-<slug>.md`. The file body is one YAML frontmatter block (every column from `memories` plus a `private` flag) followed by the raw memory content. A sibling `relations.jsonl` carries the append-only edge log. A `<root>/.cludignore` file applies a conservative privacy filter (regex body-blacklist + scope/session globs) and a `private: true` frontmatter flag wins unconditionally. Default tier policy is Semantic-only; Episodic widens via env var or CLI flag; Working never exports.

**Rationale:**
- **Git-friendly diffs.** One memory per file means a `git diff` shows exactly what changed. A monolithic JSON-lines export would re-write a 10 MB file on every save and produce unreadable diffs.
- **Human-reviewable.** YAML frontmatter + Markdown body is the format every developer already reads when looking at GitHub READMEs, Jekyll blogs, and Obsidian vaults. The user can `cat .clud/memory/semantic/*.md` and understand what is about to be committed without parsing tools.
- **Conservative privacy default.** Body-regex blacklist + scope/session globs + `private: true` override gives three independent layers. Defaults ship aggressive secret-shaped patterns (AKIA / sk- / Bearer / PEM blocks / etc.) so the failure mode of a too-loose `.cludignore` is "fewer rows on disk" not "secrets in the repo." The user-visible escape hatch (`--allow-private`) requires explicit opt-in.
- **Semantic-only by default.** Working is transient by definition (DD-016 already exempted it from auto-forget for Episodic / Semantic; same boundary). Episodic captures session-summarized text that's almost always specific to one teammate's session — not durable knowledge — so it opts in via CLUD_MEMORY_EXPORT_EPISODIC / --include-episodic. Semantic is the durable-cross-session tier; exporting it by default is what gives the format its value.
- **ULID prefix in the filename, uuidv7 id in the frontmatter.** Filename-prefix ULID gives filesystem-sortable listings ("what got exported recently?"). The canonical id stays the original uuidv7 so cross-machine `import_from_disk` is keyed on a stable id that does not change per export.
- **`relations.jsonl` append-only.** Edges are small structured records; one file with `OpenOptions::append(true)` 3-way merges trivially. The dedup-on-import path keys on `(src_id, dst_id, kind)` so hand-edits and merge artifacts are tolerated.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Single `memory.jsonl` export | Every save rewrites the whole file → noisy diffs, hostile merge conflicts. No per-row review path. |
| SQLite dump (`.dump`) committed | Not human-reviewable. Defeats the privacy-review use case. Re-imports require running SQL against a fresh store. |
| Per-tier single Markdown files | Same diff/merge problem as a single JSONL but with a less efficient parser. |
| Export every tier by default | Working tier is per-session scratch ("what did the user just type?") — leaking that to git would dump session history into the repo. Conservative default makes the format usable without configuration. |
| Whitelist-by-tag instead of blacklist | Forces the user to tag every row that should be exported. The 99% case is "this fact is generic project knowledge"; whitelisting penalizes that case. Aggressive blacklist + explicit `private: true` covers the secret-leakage failure mode without forcing per-row tagging. |
| Encrypted artifacts | Out of scope. Teams that want encryption use `git-crypt` or similar over the top of the directory; the layout doesn't preclude it. |

**Consequences:**
- Adopting the format is one `clud memory export --to-disk` away. No daemon required for the export verb (read-only WAL walk).
- `.clud/memory/` is the single directory teams add to git; `.cludignore` lives at the same level as `.gitignore`.
- Filename diffs across re-exports of the same row are noisy because the ULID prefix changes per export. Mitigation: rows whose `id` is already on disk are not re-emitted (`scan_existing_ids`). A future enhancement could stabilize the ULID prefix to the row's `created_at_ms`.
- The Stop-hook auto-export wiring is owned by sibling #260; this PR exposes `export_to_disk` as the seam.
- The format is versioned implicitly via the frontmatter shape. A future v2 will add `#[serde(rename = "schema-version")]` to the frontmatter if a breaking change is needed; today's reader uses `serde(default)` so additive frontmatter fields are non-breaking.

---

## DD-022: Hook subcommands talk HTTP to the daemon

**Context:** Issue #260 adds four `clud hook <verb>` subcommands that Claude Code / Codex invoke at session lifecycle events (`session-start`, `user-prompt-submit`, `post-tool-use`, `stop`). Each hook is a short-lived clud subprocess. The handlers need to read memory rows (for the `<context>` block on `session-start`) and write memory rows (for the `remember:` directive on `user-prompt-submit`). There are three ways to connect: (A) open the on-disk SQLite + tantivy directly, (B) re-use the existing `/memory/*` HTTP routes through the daemon, (C) invent a new IPC channel scoped to hooks.

**Decision:** Every hook handler talks HTTP to the daemon via `daemon::memory_client::{http_recent, http_save}` — the same surface the `clud memory` CLI verbs use ([DD-019](#dd-019-clud-memory-cli-verbs-proxy-mutating-ops-through-the-daemon)). No direct on-disk access from hooks, no new IPC channel. When the daemon is unreachable, hooks exit 0 silently (the agent must never be blocked by a failing hook).

**Rationale:**
- **Single SQLite writer per process.** The daemon already owns the writer ([DD-017](#dd-017-memory-service-runs-in-process-inside-the-existing-clud-daemon)). Letting hooks open the on-disk file directly would race against the daemon's `BEGIN IMMEDIATE` calls (`SQLITE_BUSY`) and against the daemon's tantivy `IndexWriter` (`LockBusy`). Same failure mode as the CLI; same fix.
- **HTTP routes are already there.** `/memory/recent` and `/memory/save` were wired live by #262/#276. Re-using them keeps the wire seam aligned with the `clud memory` CLI and the dashboard.
- **Embedder warm-start.** The daemon paid the embedder cold-start once. A hook saving a row gets the warm embedder for free; opening its own embedder per hook would cost 200ms+ per `user-prompt-submit` on a fresh daemon (#257).
- **Failure isolation.** Hooks are fired mid-session by the agent; a panicking hook can't OOM the daemon because they live in separate processes. If the daemon dies, hooks notice via TCP-connect failure and exit 0 silently.
- **Agent transcript hygiene.** Claude treats hook stdout as additional context for the current turn. The `session-start` handler is the *only* hook that writes to stdout; the other three are silent on success. A new IPC channel that surfaces errors on stdout would pollute every turn.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Hook opens SQLite + tantivy directly | Races against the daemon's writer; same failure mode as the CLI alternative ruled out in DD-019. Plus the hook would have to load its own embedder per call. |
| New IPC channel (Unix sockets / named pipes scoped to hooks) | Triples the wire-format surface (CLI, MCP, hooks) for no benefit. The HTTP routes were built for the same shapes — `/memory/recent` and `/memory/save`. |
| Embed in the MCP server (#259) instead | MCP is a JSON-RPC stream with persistent state; hooks are fire-and-forget. The two abstractions don't compose. The bridge would add a JSON-RPC envelope and stream multiplexing on the hot session-start path for no gain. |
| `clud hook` execs an in-daemon `__hook` subcommand to share state | Doubles startup latency on every hook (Rust binary startup, linker, libc). Hooks must be cheap. |

**Consequences:**
- `crates/clud-bin/src/hooks.rs` is a thin shell — payload parse, HTTP call, stdout formatter. Each runner is ~30 LOC.
- All four runners return `i32` and always return `0`. The `dispatch` entry point in `main.rs` calls `std::process::exit(hooks::dispatch(sub))` immediately after argv parse — before `console_title::set_for_current_cwd()`, before `ensure_daemon`, before any background-thread spawn. A hook fired from a tool-use call mid-session must never retitle the parent terminal or fork a daemon.
- Verbose stderr diagnostics are gated on `CLUD_MEMORY_DEBUG_HOOKS=1`. The default-off behavior keeps agent transcripts clean.
- Auto-export of recalled rows to disk and tool-output classification are deferred to siblings #264 and v0.5 respectively; the `post-tool-use` hook ships as a logged no-op in v0.1, and the `stop` hook's consolidation path is wired but waits on a `/memory/consolidate` route.
- Registration of `clud hook` calls into `~/.claude/settings.json` / `~/.codex/hooks.json` is owned by sibling #265; the four subcommands are `hide = true` in clap because they are not user-facing.

---

## DD-023: ScopeSelector third row enables memory MCP+hooks in one action; refuse-to-clobber semantics

**Context:** Issue #265 (sub-issue 10 of META #255) wires the native agent-memory subsystem into the launch flow. The MCP server (`clud mcp`) and the four `clud hook <verb>` subcommands have already shipped (#259, #260) but need to be *registered* into the user's `~/.claude.json` / `~/.codex/config.toml` (MCP) and `~/.claude/settings.json` / `~/.codex/hooks.json` (hooks) for the backend to actually invoke them. Without registration, the subsystem is dark. Three questions had to be answered: (1) where in the UX does the opt-in live, (2) what happens when the user has already hand-edited a `clud-memory` block, (3) is the registration a one-shot migration or an idempotent pass that the launch flow can run every time.

**Decision:** The existing two-row `ScopeSelector` ([`launch_setup.rs`](../crates/clud-bin/src/launch_setup.rs)) grows a third row — `Globally + clud memory (recommended)` — which is its own `LaunchSetupScope::GlobalWithMemory` variant. Selecting it runs the same actions as `Global` plus two new actions (`MemoryMcpRegistrationAction` and `MemoryHookRegistrationAction`) that upsert the MCP and hook entries idempotently into the *selected* backend's config files. The actions carry a managed marker (`_clud_managed: true` in JSON, `# managed-by: clud-memory` lead comment in TOML) and **refuse to overwrite** any `clud-memory` key that lacks the marker.

**Rationale:**

- **One toggle, not four.** Memory is *only* useful if both the MCP server and all four hooks are registered. Splitting them into separate scope-selector rows (`+ MCP`, `+ hooks`, `+ both`) would let users land in a half-configured state where the MCP server is wired but no rows ever land because the `user-prompt-submit` hook never fires the `remember:` directive. Bundling them into one row makes the failure mode "memory works or doesn't" rather than "memory silently swallows half the saves."
- **Idempotent + refuse-to-clobber instead of one-shot migration.** A one-shot migration is hostile to upgrades — bumping `clud` would either silently skip the new entry shape or require a separate "did I migrate yet?" sentinel. Running every launch is cheap (one stat + one parse per file, all under a sibling `.lock`), and the managed marker lets the action distinguish its own writes from user writes. Re-running on an up-to-date file is a no-op (`Outcome::AlreadyPresent`); rerunning with a newer `clud` version rewrites the managed block in place; rerunning against a user-defined block surfaces a `[clud] note: refusing to ...` line and continues without touching the file.
- **Selected-backend only, mirror the existing actions.** `BundledSkillsAction` is registered twice — `Backend::Claude` and `Backend::Codex` — and `setup_actions()` filters by `action.backend() == backend`. The two new memory actions follow the same shape: one Claude instance writes `~/.claude.json` + `~/.claude/settings.json`, one Codex instance writes `~/.codex/config.toml` + `~/.codex/hooks.json`. A user on Claude doesn't pay for a Codex config write they don't want, and the dispatch path stays uniform.
- **Memory-aware default.** `ScopeSelector::for_home(home)` probes `mcp_config::memory_already_registered(home)` and starts the cursor on row 2 (recommended) when memory is not yet configured, row 0 (session-only) when it is. First-time users land on the row that does the right thing without reading the spec; repeat users keep muscle memory.
- **`toml_edit::DocumentMut` over `toml::to_string`.** TOML round-trip via `toml::to_string` would re-serialize the entire `~/.codex/config.toml`, dropping user comments and reordering keys. `toml_edit` preserves trivia (comments, blank lines, key order) by design — the test `codex_toml_registration_preserves_existing_keys` is the regression guard.
- **Atomic temp-file + rename writes.** A crash mid-write would leave an empty or half-written `~/.claude.json`, which Claude Code reads on every launch. `tempfile::NamedTempFile::new_in(parent)` + `persist(path)` keeps the rename on the same filesystem (so no `EXDEV`) and the rename is atomic on POSIX and on NTFS (`MoveFileExW` semantics behind `persist`).

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Add memory MCP/hook to the existing `Global` row | Existing users who pick `Globally` today don't expect new persistent edits to their config files. A separate row makes the opt-in explicit and the rollback path obvious (re-run, pick `Globally`). |
| Silently overwrite user-edited `clud-memory` blocks | User picked a custom command (`/usr/local/bin/clud-fork`, a forked branch, a wrapper script) for a reason. Overwriting it on every launch would actively fight the user. The refuse-to-clobber + `[clud] note: ...` line surfaces the conflict without blocking the launch. |
| One-shot migration gated by a sentinel file | Adds a "did I migrate yet?" state machine that the agent has to reason about on upgrade. The idempotent pass collapses to the same end state without the sentinel. |
| Serialize TOML via `serde::Serialize` + `toml::to_string` | Drops user comments, reorders keys, loses inline trivia. The `toml_edit` round-trip preserves all of that and the regression test (`codex_toml_registration_preserves_existing_keys`) pins it. |
| Register into every detected backend (mirror `BundledSkillsAction`) | Memory is more invasive than skills (permanently edits user MCP/hook config). Registering only into the currently selected backend minimizes blast radius; the user can rerun `clud --claude --setup` and `clud --codex --setup` if they want both. |

**Consequences:**

- The selector default flips between row 0 and row 2 based on `memory_already_registered`. First-time users get the recommended path; repeat users land on the row they last picked. The `for_home(home)` constructor is the test seam.
- Four advisory locks under `~/.clud/memory-{claude,codex}-{mcp,hooks}.lock` serialize concurrent launches; `fs4` releases them on process exit. The `.lock` file itself stays on disk (cosmetic; same as `codex_hook_normalize`).
- `clud --setup --dry-run` prints a four-file summary without writing — useful for users to preview what the action will touch.
- The `MemoryConfig` variant added to `SetupError` is propagated through `main.rs`'s `[clud] note: global setup failed: ...` path, matching the existing skill/hook setup failure handling. Setup failures are non-fatal.
- `clud memory uninstall` (a future verb under META #255) re-uses the symmetric `remove_*` helpers in `mcp_config.rs` to strip the managed entries while preserving any user-defined siblings — the lock pattern and atomic write keep the operation safe even mid-session.
