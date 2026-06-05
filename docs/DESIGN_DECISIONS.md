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
