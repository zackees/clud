# clud Design Decisions

ADR-style records for non-obvious design choices in clud. Each entry follows the structure: Context, Decision, Rationale, Alternatives Considered, Consequences.

Decisions are numbered for stable cross-references (e.g. `DD-005`). Numbers are append-only; superseded decisions stay in place with a "Superseded by" note.

---

## DD-001: Rust binary distributed as a Python wheel via maturin `bindings = "bin"`

**Context:** clud is a CLI that orchestrates other CLIs (`claude`, `codex`) on Windows, Linux, and macOS. Its distribution channel needs to reach Python developers (the primary audience already running `pip install` for AI tooling) without forcing them to install a Rust toolchain or hand-pick a binary for their platform.

**Decision:** Implement clud as pure Rust binaries in `crates/clud-bin`, then package and distribute them as a Python wheel using `maturin` with `[tool.maturin] bindings = "bin"`. Installing the wheel places the native `clud` executable and helper executables such as `clud-block-bad-cmd` onto the user's `PATH`. The Python package (`src/clud/__init__.py`) is a thin version shim with no runtime code.

**Rationale:**
- Single artifact per platform: `pip install clud` works the same on Windows, macOS, and Linux without users picking a binary.
- maturin's `bindings = "bin"` is the supported way to ship CLI binaries through PyPI; no custom wheel-building code needed.
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

**Decision:** Unless the user explicitly passes `--safe`, clud injects the effective harness's non-interactive permission flag: `--dangerously-skip-permissions` for Claude or `--dangerously-bypass-approvals-and-sandbox` for Codex. This applies to every harness invocation (interactive, loop, daemon).

**Rationale:**
- Users reach for clud specifically to skip per-call prompting. Defaulting to "prompt for everything" would defeat the purpose.
- The opt-out (`--safe`) is one word, easy to remember, and preserves the safe path for users who want it.
- A single decision point in `command::build_launch_plan_for_target` means there is no path that forgets to apply the policy.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Off by default, opt-in `--yolo` | Most invocations would need `--yolo`, adding noise and creating muscle memory that defeats the safety value of the off-by-default. |
| Per-backend default (claude on, codex off) | Inconsistent UX; hard to explain. |
| Read from a config file | Adds a hidden global setting; behavior depends on machine state. |

**Consequences:**
- New users must be told about `--safe` (covered in `README.md`).
- Any production code path that bypasses `build_launch_plan_for_target` would silently lose YOLO injection; see [DD-005](#dd-005-single-launchplan-as-source-of-truth-for-everything-clud-runs).

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

**Decision:** clud detects which backends are on `PATH` and supports either via `--claude` / `--codex` flags. The `Backend` enum is plumbed through every code path that constructs argv and every persistent launch-setup action. Where backends diverge (`--model` placement, `-p` semantics, `stream-json` injection, the `exec`/`resume` keywords), the divergence is encoded inside `command/`. Skills bundled into the `clud` binary install to `~/.claude/skills/` for Claude Code and `~/.codex/skills/` for Codex (mirrored layout), only during global launch setup for the selected backend; stale clud-managed copies under the retired `~/.agents/skills/` path are purged best-effort during Codex global setup (see [DD-013](#dd-013-codex-skills-install-to-codexskills-mirror-of-claude)).

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
- The skill system needs to handle two install targets; the single installer that serves both is [DD-039](#dd-039-bundled-skills-have-exactly-one-source-of-truth).

---

## DD-005: Single `LaunchPlan` as source of truth for everything clud runs

**Context:** clud has many code paths that need to know "what argv will clud actually run with these flags?" — the runner itself, the daemon worker, `--dry-run` JSON output for tests, the loop iteration loop, hook health remediation, and so on. Each path independently reconstructing argv is a recipe for divergence (one path forgets YOLO injection, another places `--model` in the wrong slot).

**Decision:** Every production code path goes through `command::build_launch_plan_for_target` and consumes the resulting `LaunchPlan` struct (`crates/clud-bin/src/command/types.rs`). The older `command::build_launch_plan` function remains only as a native-harness compatibility wrapper for tests and callers that have not yet adopted resolved launch targets. The struct carries the executable argv (including prompt arguments), working directory, optional loop markers, optional repeat schedule, and resolved provider/harness metadata. The daemon serializes the complete plan; `--dry-run` emits a stable JSON projection of the same plan. The runner, daemon worker, and remediator each consume the plan.

**Rationale:**
- One implementation of "what runs" means no drift between dry-run output and actual execution.
- Tests that exercise plan construction (via `--dry-run`) automatically exercise the same path runtime uses.
- Adding a new code path that needs argv is mechanical: resolve a launch target, call `build_launch_plan_for_target`, and consume the struct.

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
| Use named pipes / Unix sockets directly | Platform-specific code; TCP loopback is portable and equally fast for this workload. **Superseded by [DD-025](#dd-025-the-broker-frame-lane-is-the-default-daemon-transport-superseding-dd-006s-tcp-only-rationale) — this is now the default path.** |
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

**Status:** Superseded by [DD-039](#dd-039-bundled-skills-have-exactly-one-source-of-truth) and [DD-040](#dd-040-clud-pr-clud-fix-clud-do-and-clud-pr-merge-are-retired-in-favor-of-goal). The body below is kept as history; `skill_install.rs` and the top-level `skills/` tree no longer exist.

**Context:** Skills are slash-commands (`/clud-pr`, `/clud-issue`, etc.) bundled into the `clud` binary via `include_str!` and installed into the user's backend home(s) during global launch setup. Session-only launches do not write persistent skill files. Two installer implementations exist in the codebase today:

- `src/skills.rs` - multi-backend (`~/.claude/skills/`, `~/.codex/skills/` gated by `~/.codex`), non-overwriting (preserves user edits), reads from `crates/clud-bin/assets/skills/`, and purges stale clud-managed copies from `~/.agents/skills/` (see [DD-013](#dd-013-codex-skills-install-to-codexskills-mirror-of-claude)).
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

1. **Cooperative flag.** `startup::install_ctrlc_flag()` installs a Ctrl+C handler that sets a shared `Arc<AtomicBool>`. The flag is consumed by the iteration loop in `runner.rs`, the daemon attach loop in `daemon/attach.rs`, and the GC scanner thread in `gc/scanner.rs`. Each polling site checks the flag and exits gracefully.
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

## DD-013: Codex skills install to `~/.codex/skills/`, mirror of Claude

**Context:** Clud bundles `SKILL.md` playbooks for `/clud-issue`, `/clud-review`, etc. inside the binary and writes them to per-backend user directories during global setup. PR #243 (closing issue #241) moved the Codex install target from `~/.codex/skills/` to `~/.agents/skills/`, on the belief that Codex had adopted a shared cross-vendor `~/.agents/` convention. In practice, Codex CLI loads skills from `~/.codex/skills/` and never consulted `~/.agents/skills/`. The visible symptom: `clud --codex -p "/clud-issue <issue>"` did not resolve `/clud-issue` even though the SKILL.md was installed — Codex never looked at the file. Reported in #289; meta burn-down at #299.

**Decision:** Codex skills install to `~/.codex/skills/<name>/SKILL.md`, the same layout Claude uses at `~/.claude/skills/<name>/SKILL.md`. Existing clud-managed copies under `~/.agents/skills/` are purged best-effort on first Codex global setup after upgrade (`purge_stale_agents_skills` in `skills.rs`). The purge applies the same conservative rules as the prior `~/.codex/skills/` purge: only delete a `SKILL.md` that contains the `managed-by: clud` marker and lives under a currently bundled skill name; leave unrelated files and user-authored skills alone.

**Rationale:**

- The whole point of installing the file is for the backend to find and execute it. Installing somewhere the backend ignores is worse than not installing at all — it consumes disk, suggests false coverage in tests, and masks the real bug.
- Mirroring Claude's layout eliminates a backend-specific branch in `SKILL_BACKENDS`: both entries now use `skills_home_subdir: None` (the field stays for future backends whose skills live outside their config root).
- Skip-if-exists still preserves user-edited skills at the new location.
- The cleanup of `~/.agents/skills/` is symmetric to the prior `~/.codex/skills/` cleanup pattern, so users upgrading don't end up with stale duplicates.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Keep installing to `~/.agents/skills/` and add runtime slash-command expansion inside `push_prompt` (intercept `/clud-issue ...` and inline the SKILL.md body before passing to `codex exec`) | Doubles the surface area (install path + runtime translation), tightly couples `command/prompts.rs` to skill discovery, and gives nothing for interactive Codex users. The install-to-the-right-place approach is strictly simpler. |
| Install to both `~/.codex/skills/` and `~/.agents/skills/` | Two copies on disk drift apart over time when users edit one. No real consumer of `~/.agents/skills/` has been identified. Add a second target only when a real need surfaces. |
| Install to `~/.codex/prompts/<name>.md` (Codex's documented custom-prompts location) | Requires a different format (plain markdown, no YAML frontmatter, no trigger metadata) and loses skill semantics. Worth revisiting separately if Codex's skill loader ever changes. |

**Consequences:**

- `clud --codex -p "/clud-issue 123"` works end-to-end on first global setup after upgrade.
- Users currently holding clud-managed copies under `~/.agents/skills/` see them removed on the next Codex global setup. User-authored content under that path is preserved.
- `SKILL_BACKENDS` Codex entry now sets `skills_home_subdir: None`. The `skills_home_subdir` field remains on `SkillBackend` for future backends that need it; a unit test (`skills_dir_honors_skills_home_subdir_override`) keeps that contract exercised.
- Reverses the install-path decision made in #241/#243 but retains the symmetric one-time cleanup behavior, just pointed at the other directory.

**Verification (added 2026-06-07, Codex CLI 0.137.0, closes #290):**

Three independent lines of evidence confirm Codex CLI loads skills from `~/.codex/skills/`, not `~/.agents/skills/`:

1. **Embedded path literals in the Codex binary.** Running `strings` on `codex.exe` (npm package `@openai/codex@0.137.0`, file `vendor/x86_64-pc-windows-msvc/bin/codex.exe`) finds the literal path:
   ```
   ${CODEX_HOME:-$HOME/.codex}/skills/.system/imagegen/scripts/remove_chroma_key.py
   ```
   Codex's own built-in `imagegen` system skill lives under `$HOME/.codex/skills/.system/`. The skill loader does not look at `~/.agents/skills/`.
2. **System-skills marker.** The same binary contains the strings `create system skills subdir`, `create system skills file parent`, `write system skill file`, and `.codex-system-skills.marker` — all rooted at `$HOME/.codex/skills/`.
3. **Plugin/skill telemetry types.** Symbols like `codex_app_server_protocol::protocol::v2::plugin::SkillsListParams`, `SkillsExtraRootsSetParams`, and `SkillsConfigWriteParams` confirm `~/.codex/skills/` is the canonical root, with extra roots optionally configurable on top (not the other way around).

`~/.agents/skills/` appears nowhere in the Codex binary's path literals. The pre-#243 layout was the right one all along.

Note: this entry replaces what would have been [#290](https://github.com/zackees/clud/issues/290)'s separate verification spike — the binary-strings evidence is stronger than a black-box repro run, since it shows the source-of-truth path Codex's loader was built against.

---

## DD-014: Repo-scoped clud config lives at `.clud/settings.json` (mirrors `.claude/settings.json`)

**Context:** zackees/clud#343 wires up a repo-scoped opt-in marker so that when a developer checks out a repo, `clud` can transparently route Rust toolchain calls (cargo / rustc / rustfmt / clippy-driver / rustdoc) through [soldr](https://github.com/zackees/soldr) by prepending soldr's shim dir to the session `PATH`. The design needs a single, unambiguous file at the repo root that:

1. Declares the opt-in (presence + explicit field).
2. Carries forward-compatible structured fields (the `rust` section: `use_soldr`, `install`, optional `version` pin — and room for future `python`, `js`, etc.).
3. Doesn't collide with existing repo dot-conventions.
4. Reads symmetrically with the `.claude/` convention developers using Claude Code already know.

Earlier drafts considered `.clud` (bare file), `.clud.toml`, and `.clud/config.toml`. All three either collided with an existing path (the `.clud/` directory was previously gitignored and used for `/clud-loop` runtime state) or broke symmetry with the `.claude/settings.json` pattern.

**Decision:** Put the file at `.clud/settings.json`. The `.clud/` directory is now tracked (not blanket-gitignored). Inside it:

- `.clud/settings.json` — tracked. The repo-scoped opt-in marker + structured config.
- `.clud/settings.local.json` — gitignored. User-local overrides (mirrors `.claude/settings.local.json`).
- `.clud/loop/` and any other runtime state — gitignored via `.clud/*` plus `!.clud/settings.json` allowlist.

Parser lives in [`crates/clud-bin/src/repo_clud_config.rs`](../crates/clud-bin/src/repo_clud_config.rs); session activator lives in [`crates/clud-bin/src/soldr_activate.rs`](../crates/clud-bin/src/soldr_activate.rs); main.rs calls `soldr_activate::activate_soldr_shims_if_requested()` right after `trampoline::unlock_exe()`.

Schema (v1 activation shape):

```json
{
  "rust": {
    "use_soldr": true,
    "install":   true,
    "version":   "0.7.55"
  }
}
```

`clud optimize rust` also writes the equivalent current-main shape under
`optimize.rust`:

```json
{
  "optimize": {
    "rust": {
      "use_soldr_shims": true,
      "install_soldr": true
    }
  }
}
```

The parser accepts both forms. Direct `rust` keys win over `optimize.rust`
keys inside the same file; repo-level values still win over user-level values
per field. Omitting the version is the rolling-latest policy; `"latest"` is an
equivalent case-insensitive alias. A numeric version remains an exact pin.

**Rationale:**

- **Symmetry with `.claude/settings.json`.** Developers using Claude Code already understand `.claude/settings.json` as the "tracked, repo-scoped, JSON" config + `.claude/settings.local.json` as "gitignored local overrides". `.clud/settings.json` reuses that mental model verbatim. The `.gitignore` allowlist pattern is identical (`.clud/*` + `!.clud/settings.json` + `.clud/settings.local.json`).
- **Directory, not bare file.** `.clud/` as a directory lets us grow new files later (`hooks/`, `commands/`, `agents/`, runtime state under `loop/`) without inventing a second top-level marker.
- **JSON, not TOML.** JSON matches `.claude/settings.json` and the newer `~/.clud/settings.json` global settings file. `.clud/settings.json` may be generated or edited by tools, so JSON's strict syntax (no comments, explicit quoting) is the right trade-off when both humans and machines read/write it.
- **`rust` nesting from day one.** Even though only the Rust activation section exists today, scoping under `"rust"` means future `"python"` / `"js"` sections don't collide with `"use_soldr"` style top-level keys.
- **Soldr stays passive.** Soldr exposes only `soldr shims --json`. clud is the active consumer: clud reads `.clud/settings.json`, decides whether to call soldr, prepends `PATH`. Soldr knows nothing about `.clud/settings.json`. This dependency direction lets soldr-only consumers (no clud) call `soldr shims --json` themselves from any setup script.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| `.clud` (bare file at repo root) | Collides with the pre-existing `.clud/` directory used by `/clud-loop` for runtime state. Either every consumer has to handle file-vs-dir ambiguity per-checkout, or we ship a migration. Cleaner to use the directory we already have. |
| `.clud.toml` (file, distinct from `.clud/` dir) | No directory growth path. We'd need a second marker the moment we want `.clud/hooks/` or `.clud/commands/`. Splits the convention across two top-level paths. |
| `.clud/config.toml` (TOML inside the dir) | Loses symmetry with `.claude/settings.json`. Developers already know the `.claude/` layout; the `.clud/` layout should read the same way without forcing a second mental model. |
| Reuse `.claude/settings.json` with a new `"clud"` section | Crosses tool ownership. `.claude/settings.json` is Claude Code's file; adding clud-specific keys to it makes both tools' configs fragile to the other's schema evolution. clud should own its own file. |
| `~/.clud/settings.json` (user-level only, no repo file) | Misses the per-repo opt-in case — a developer who wants soldr routing for one Rust repo but not another can't express that with a user-level setting alone. The user-level file (owned by `clud_settings.rs`) and the repo-level file (`.clud/settings.json`, this DD) coexist; repo wins per field for soldr activation. |

**Consequences:**

- **`.gitignore` change.** The `.clud/` blanket-ignore is replaced by `.clud/*` + `!.clud/settings.json` + `.clud/settings.local.json` (mirroring `.claude/*`). Existing `/clud-loop` runtime state under `.clud/loop/` stays gitignored via the wildcard.
- **Session startup grows a fixed-cost probe.** `discover_repo_clud_config()` does an O(1) `fs::metadata` per parent dir up to the `.git` boundary. Negligible (~tens of microseconds), but it's a new mandatory step in the startup path. Repos without `.clud/settings.json` pay only the directory-walk; no `soldr` spawn happens.
- **Soldr's own `.clud/settings.json` is its dogfood.** This PR adds a `.clud/settings.json` to the clud repo itself declaring both `rust.use_soldr = true` and the current `optimize.rust.use_soldr_shims = true` shape, so every clud contributor's session automatically routes cargo through soldr per CLAUDE.md.
- **Global settings must opt in explicitly.** `~/.clud/settings.json` now stores many unrelated clud preferences. The activation parser ignores a user-level file unless it contains a soldr directive (`rust.*` or `optimize.rust.*`), preventing unrelated global settings from enabling soldr in every repo. Repo-level `.clud/settings.json` remains the presence-based opt-in marker for #343.
- **Reversal cost is moderate.** Renaming to a different filename later is a one-PR rename. Switching to TOML would mean a parser swap and rewriting `.clud/settings.json` to `.clud/settings.toml` everywhere — also one PR. Schema additions are append-only thanks to `#[serde(default)]` on every field.

**Verification:** `crates/clud-bin/src/repo_clud_config.rs` ships unit tests covering:

- Empty repo file = defaults (presence-only contract).
- Missing `rust` section = defaults for repo files (forward-compat for future sections).
- `optimize.rust` aliases emitted by `clud optimize rust`.
- Direct `rust` keys win over `optimize.rust` aliases.
- Unrelated user-level settings do not enable global soldr activation.
- Explicit `use_soldr=false` honored.
- Discovery walks up from a subdirectory.
- Discovery stops at the `.git/` boundary (no cross-repo bleed).
- Malformed JSON warns + returns `None`.

`crates/clud-bin/src/soldr_activate.rs` covers the activator failure-mode contract per zackees/clud#343.

## DD-015: Uncovered-disk-sink sweeps are env-var-gated, background-threaded, and disk-pressure-prioritized

**Context:** zackees/clud#511 (rolling up #509 + #510) closes the two biggest holes in clud's disk reclamation: the OS temp scatter of a session's backend agent, and stale Rust `target/` output under dev roots. Neither has a redb registry row, so the tracked-entry GC never sees them. The daemon already runs filesystem-only sweeps (uv-cache, #423), which is the pattern these extend.

Three questions had non-obvious answers:

1. **Config surface.** The issues sketched a typed `settings.json` section. But every existing knob in this exact subsystem (`CLUD_GC_TICK_SECS`, `CLUD_GC_WARN_FREE_GB`, `CLUD_GC_MIN_AGE_HOURS`, …) is an env var read in `gc_service.rs`. Adding typed settings + `KNOWN_TOP_LEVEL_KEYS` plumbing for these would have been net-new surface inconsistent with the neighbors.

2. **Blocking.** A `target/` walk over several dev roots can take real wall-clock and does `remove_dir_all`. Running it inline in the registry tick loop would stall unrelated GC ops (worktree/extern purges, the disk watchdog).

3. **When to run.** Reclamation should be aggressive under disk pressure but must not compete with an active build for CPU the rest of the time.

**Decision:**

- **Env-var config**, matching the subsystem convention: `CLUD_SESSION_TMP` (opt-out, default on), `CLUD_GC_TARGET_ROOTS` (opt-in; unset ⇒ target sweep is a no-op), `CLUD_GC_TARGET_STALE_DAYS` (default 14), `CLUD_GC_SWEEP_MAX_CPU_PCT` (default 60). Sweep logic lives in `crate::gc::{session_tmp,target_sweep}`; the daemon schedulers (`daemon/{session_tmp_sweep,target_sweep}.rs`) mirror `uv_cache_sweep`'s sentinel-throttle shape.
- **Background thread.** The tick calls `spawn_maintenance_sweeps`, which fans the two heavy sweeps onto a detached `clud-gc-sweep` thread guarded by an `AtomicBool` (no overlapping sweeps). The registry tick loop returns immediately.
- **Prioritization** (`maintenance_action`, pure + unit-tested): disk low (free below `CLUD_GC_WARN_FREE_GB` on the `~/.clud` volume or any target root) ⇒ run now, bypassing the per-sweep sentinel; otherwise run only when global CPU is under the ceiling, else defer to the next tick. The ~200ms CPU sample runs on the background thread, never the tick.

**Session temp default-on** is deliberate (the user asked for the redirect to be the default behavior), but every failure path is soft: no home dir, unwritable volume, or `CLUD_SESSION_TMP=0` all just leave the OS temp dir in place — a session launch never fails because of this. **Target reclamation default-off** because, unlike disposable temp, dropping `target/` forces a rebuild; the 14-day mtime gate is the cheap stand-in for "no live build owns this."

The `SESSION_TMP_STALE_AFTER` (48h) and `target_sweep` day-gate are **separate constants** from `PERIODIC_GC_WORKTREE_STALE_AFTER`, not a shared symbol — the policies only coincide in value today and will diverge.

See [gc-and-registry.md → Filesystem sweeps](architecture/gc-and-registry.md#filesystem-sweeps-non-registry).

## DD-016: `bad_commands` — generic, config-driven "bad command → blessed replacement" rules in `.clud/settings.json`

**Context:** zackees/clud#519. The `block-bad-cmd` PreToolUse hook (`crates/clud-bin/src/block_bad_cmd.rs`) already enforced one hardcoded rule shape — bare Rust-toolchain calls (`cargo`, `rustc`, …) are denied with a message telling the agent to prefix with `soldr`. Other repos need the identical enforcement shape for entirely different, repo-specific command pairs (motivating example: banning bare `playwright` in favor of a project's faster `npm run test:integration` pipeline) — a rule that has nothing to do with Rust and can't live in clud's compiled binary.

**Decision:** Add a `bad_commands` array to `.clud/settings.json` (see DD-014 for the two-level user/repo config this extends). Each entry:

```json
{
  "bad_commands": [
    {
      "id": "no-raw-playwright",
      "match": "playwright",
      "match_mode": "glob",
      "replacement": "npm run test:integration",
      "reason": "use the blessed pipeline; raw playwright is slower",
      "passthrough_prefixes": ["soldr"],
      "allow_override": true
    }
  ]
}
```

- **`match`** — a pattern for the normalized program-name token (`program_name(words[0])`), never the raw command line. This is deliberate: matching only the head token is what makes `rg playwright` / `grep -r playwright .` (searching *for* the word) correctly stay allowed, since their head token is `rg`/`grep`, not `playwright`. `match_mode` is `"glob"` by default (`*`/`?`/`[...]`, always whole-token-anchored — never a substring/prefix match) or `"regex"` to opt one rule into a raw regex pattern (also whole-token-anchored automatically).
- **`replacement`** / **`reason`** — populate the deny message: `"{reason} Use `{replacement}` instead."`.
- **`passthrough_prefixes`** (optional, same `match_mode` as the rule) — soldr-style transparent wrappers. When the current head token matches one of a rule's own passthrough prefixes, *that rule* is excluded from the rest of the segment's evaluation and the scan advances to the next token — so `soldr playwright run` is allowed for a rule that lists `soldr` as a passthrough prefix, without blanket-exempting *other* rules from matching whatever `soldr` wraps.
- **`allow_override`** (optional, default `false`) — per-rule opt-in for the override escape hatch: `CLUD_BAD_CMD_OVERRIDE="<rule-id>:<reason>"` set as a **real process environment variable** (never parsed out of the command text — text-parsing it would race the hook's own env-assignment stripping in `command_words()`). The reason is mandatory; a missing/empty reason is treated as no override. Every accepted or rejected override attempt is logged.

**Merge semantics differ from the scalar `rust.*` fields:** `bad_commands` **concatenates** repo-level and user-level rules (both are active) rather than repo-overrides-user per field, since two independent rule sets should compose. Rules are deduped by `id` — a repo-level rule sharing an `id` with a user-level rule replaces it wholesale; `id`-less rules never dedupe. `has_directive` (renamed from `has_soldr_directive`) now also treats a non-empty `bad_commands` array as a valid activation signal, so a user-level file containing only `bad_commands` (no `rust` key) still counts.

**Command-substitution / nested-shell recursion:** the existing per-segment scan (chaining on `;`/`&&`/`||`/`|`, nested `bash -c`/`cmd /c`/`powershell -Command` unwrapping) is reused as-is for generic rules. It's additionally extended to recurse into `` `...` `` / `$(...)` command substitution (excluding `$((...))` arithmetic expansion), `<(...)`/`>(...)` process substitution, and `eval "..."`, bounded by a recursion-depth cap (`MAX_SUBSTITUTION_RECURSION_DEPTH = 8`) that fails open (allows + logs) rather than denying or risking a stack overflow on pathological input — this hook is a friction-reducing nudge for a cooperative agent, not a security sandbox. Deliberate evasion (variable indirection, encoded/computed command strings, alternate-interpreter smuggling) is explicitly out of scope. Heredoc bodies (`<<'EOF' ... EOF`) are stripped before segment-scanning so their contents are never treated as invocations.

**Relationship to the hardcoded Rust rules:** `RUST_TOOLS` / `LEGACY_RUST_TRAMPOLINES` / the hybrid-`uv run` heuristic stay as their own hardcoded fast path, not migrated into the generic rule format — they carry bespoke logic and deny wording asserted verbatim by existing tests that doesn't cleanly fit a flat matcher→replacement→reason→passthrough→override shape. Generic rules run as an *additional* check in the same per-segment loop.

**Verification:** `crates/clud-bin/src/block_bad_cmd.rs` and `crates/clud-bin/src/repo_clud_config.rs` ship unit tests covering positional (not substring) matching, chaining/segment scanning, nested-shell and command-substitution recursion, arithmetic-expansion exclusion, glob vs. regex `passthrough_prefixes`, the override env-var contract (id match, mandatory reason, per-rule opt-in), config concatenation/dedup, and non-regression on the full pre-existing hardcoded-Rust test suite.

## DD-017: Dangerous arguments use token predicates; dangerous pipelines are separate rules

**Context:** zackees/clud#526. Executable-only rules cannot distinguish safe and dangerous invocations of the same program (`git push` vs. `git push --force`), and raw command-line regexes would reintroduce quoting and substring false positives that DD-016 deliberately avoided. Some hazards are relationships between processes (`curl ... | sh`), not properties of either executable alone.

**Decision:** A `bad_commands` entry may add an `arguments` object evaluated against the already-tokenized arguments after the executable. String patterns are whole-token, case-insensitive globs. A single pattern may instead use `{"match":"...","match_mode":"regex"}`; mode is local to that pattern rather than inherited from the executable rule.

```json
{
  "bad_commands": [
    {
      "id": "no-force-push",
      "match": "git",
      "arguments": {
        "ordered": ["push"],
        "any": ["--force", "-f"],
        "none": ["--force-with-lease"]
      },
      "replacement": "git push --force-with-lease",
      "reason": "unconditional force pushes can overwrite remote work"
    },
    {
      "id": "no-recursive-root-delete",
      "match": "rm",
      "through_wrappers": ["sudo"],
      "arguments": {
        "all": ["/"],
        "any_of": [
          {"short_flags_all": ["r", "f"]},
          {"all": ["--recursive", "--force"]}
        ]
      },
      "replacement": "inspect the target and delete a narrower path"
    }
  ]
}
```

Predicates present in one object combine with AND. `prefix` is contiguous from the first argument; `ordered` permits intervening arguments; `contiguous` requires adjacency anywhere; `any`/`all`/`none` have their ordinary quantifier meanings; `any_of` ORs complete nested predicate objects. `short_flags_any` and `short_flags_all` are an explicit opt-in to POSIX short-option bundle interpretation, so `-rf`, `-fr`, and `-r -f` can be equivalent without assuming every CLI bundles short options. Recursive `any_of` parsing is capped at eight levels and malformed nested patterns skip only their containing rule.

`through_wrappers` is limited to parsers clud understands (`sudo`, `env`, `command`, `exec`). In particular, `sudo -u root rm ...`, `env -u HOME rm ...`, and `exec -a alias rm ...` consume wrapper option values before matching `rm`; `env -S` tokenizes its explicit split-string value. The previously supported `env`/`command`/`exec` wrappers remain universally transparent for backward compatibility, while `sudo` requires explicit rule opt-in. Arbitrary user-defined wrapper grammars are rejected rather than guessed.

Pipeline relationships live in a sibling `bad_pipelines` array. Stages are ordered and contiguous within a single-pipe chain; `;`, `&&`, and `||` terminate the chain. The lightweight shell scanner honors quoted pipes, comments, and the active dialect's escape character: Bash/POSIX (`\`), PowerShell (backtick), or cmd (caret). The hook tool selects the initial dialect: explicit tool names win, while Codex's generic `Shell`/`shell_command` maps to PowerShell on Windows and POSIX elsewhere. Explicit nested `bash`/`pwsh`/`cmd` wrappers switch dialects for their inner command. This avoids both literal-pipe false positives and cross-dialect escape bypasses. Each stage uses the same executable and optional argument matcher shape.

```json
{
  "bad_pipelines": [
    {
      "id": "no-download-to-shell",
      "stages": [
        {"match": "curl"},
        {"match": "^(?:ba)?sh$", "match_mode": "regex"}
      ],
      "replacement": "download the script, inspect it, then run it",
      "reason": "piping downloaded content into a shell hides executed code"
    }
  ]
}
```

Both arrays concatenate across user and repo settings and dedupe by `id` with the repo definition winning. Pipeline rules share the existing per-rule override behavior. Matching embedded programs (`python -c`, encoded `eval`, generated scripts), variable indirection, and deliberate evasion remain out of scope: these rules are cooperative guardrails, not a security sandbox.

## DD-018: PTY pump uses a dedicated stdout-writer thread fed by an unbounded channel, not a smaller output-read timeout

**Context:** zackees/clud#538. `run_raw_pty_pump_full_verbose`'s single loop did `read_chunk_impl → OSC-strip → write_all → flush()` to the real terminal *before* draining stdin each turn. Under high CPU load (the reported symptom, `clud --codex`), a slow terminal `flush()` blocked the same loop turn that would otherwise forward the next keystroke, and the loop's cadence was floored at the output read's 10 ms timeout regardless. Shrinking the timeout alone doesn't fix it — the write+flush is still inline ahead of stdin, so a genuinely slow terminal still stalls forwarding for however long `flush()` takes, timeout notwithstanding.

**Decision:** Split output handling onto two dedicated threads, opened via `std::thread::scope` (`process: &NativePtyProcess` is a borrow, not an owned `Arc`, and its fields are already internally `Mutex`/`Atomic`-guarded for concurrent access — see the existing `daemon/worker.rs` reader+writer pair):

- A **reader thread** calls `read_chunk_impl`, OSC-strips, and coalesces anything else already queued via non-blocking `read_chunk_impl(Some(0.0))` before sending once over an **unbounded** `mpsc` channel. Unbounded is load-bearing: `send` must never block, or a stalled writer would eventually stall the reader (and transitively the shutdown-detection path) exactly like the bug being fixed — just one hop removed instead of zero. The tradeoff is an unbounded memory backlog if the sink stalls indefinitely; acceptable because PTY output is bounded by what the child actually writes, and the writer thread only stalls on genuinely slow real terminals, not indefinitely.
- A **writer thread** (`run_output_writer`) blocks on the channel, drains everything else pending with `try_recv()`, and issues exactly one `write_all` + one `flush()` per wakeup — turning a burst of N chunks into O(1) syscalls instead of N unbuffered writes.
- The **main thread** never touches output. It blocks on `stdin_rx.recv_timeout(STDIN_IDLE_POLL)` (5 ms) instead of the old output-read timeout; `recv_timeout` wakes immediately once a chunk is sent, so the 5 ms bound only governs idle re-polling of resize/hooks/exit, not keystroke latency.

The destination writer is a generic parameter (`W: Write + Send`) rather than hardcoded `io::stdout()`, via a `#[doc(hidden)]` `..._for_test` seam — tests inject a slow or counting sink to verify the decoupling and the O(1)-flush property without needing to control real terminal I/O timing.

**Alternatives rejected:**
- *Just lower the 10 ms timeout.* Doesn't address the root cause (write+flush is still inline and blocking); only shrinks the idle floor, not the stall-while-flushing floor.
- *Bounded channel with a small capacity.* Reintroduces the coupling this fix removes — once the bound fills, `send` blocks and the reader (and eventually shutdown detection) stalls behind the same slow writer.
- *`Arc<NativePtyProcess>` + `thread::spawn` instead of `thread::scope`.* Would work, but `NativePtyProcess` is already passed around by borrow throughout `session.rs`, and every existing pump variant/test takes `&NativePtyProcess`; `thread::scope` gets the same concurrent-thread guarantee without changing that signature or adding reference counting.

---

## DD-019: Idle CPU measurements are standalone, machine-local baselines with opt-in budgets

**Context:** Idle-cost fixes in #542 need a repeatable end-to-end signal for
both client sessions and the detached daemon. A normal pytest case cannot own
that role: its global 90-second timeout is shorter than a representative
60-second sample plus setup and teardown, and absolute CPU measurements on
shared CI runners are noisy.

**Decision:** Keep the harness in `bench/idle_cpu` as `python -m
bench.idle_cpu.harness`. It uses the integration suite's mock-agent pattern to
start a fresh daemon and detached non-PTY sessions, samples cumulative
per-process CPU time through `psutil`, and counts appended daemon-event lines.
The report is JSON; its pure assembly and budget comparison live in
`report.py` and are covered by ordinary fast pytest tests. Committed N=1 and
N=8 reports are local reference baselines. Budget enforcement is explicitly
opt-in through `--budget` or `CLUD_BENCH_BUDGET=1`, allowing 20% CPU variation
and one event line.

**Consequences:** The harness is suitable for a quiet developer machine or a
scheduled dedicated runner, never default CI. Baselines must be refreshed only
with a documented, intentional idle-cost change. Once #543 and #544 remove the
current no-op GC event stream, its zero/near-zero event budget becomes a direct
regression guard for that churn.

---

## DD-020: The soldr build backend is pinned exactly, and CI's toolchain pin is asserted to match it

**Context:** `pyproject.toml` declared `requires = ["soldr>=0.8.27"]` with
`build-backend = "soldr"`. That line resolves the *build backend* from PyPI
independently, at build time — it is a different resolution from
`setup-soldr`'s `version:` input, which provisions the *toolchain* soldr in
CI. The two look like the same pin and behave nothing like it.

soldr 0.8.26 shipped a regression (zackees/soldr#1934) that made `cargo
metadata` fail before any compilation. Within hours every clud lane was red,
**including branches whose `setup-soldr` version was pinned to the known-good
0.8.25** — the failing shim path in those logs was `.../v0.8.26/shims/rustc`,
a version nothing in the repo asked for. The floor pin offered no protection
because 0.8.26 satisfied it, and neither would `<0.9` have: soldr's *patch*
releases carry build-system behaviour changes, so a compatible-release bound
is the wrong shape for this dependency.

The drift was also unobserved. `tests/test_packaging_metadata.py` did assert
on CI's soldr versions, but by substring (`"0.8.0" in line`), which
`"0.8.27"` does not contain — so the only line it actually inspected was
`dylint.yml`'s, and that lane had quietly sat on 0.8.0 while the rest of CI
moved to 0.8.27.

**Decision:** Pin the backend exactly (`soldr==0.8.28`, the current release)
and treat the backend pin, every checkable `setup-soldr` version under
`.github/`, and `./install`'s default as **one decision with three
spellings**. A composite action may forward an input into `with.version`, but
that input must have a literal default that can be compared with the pin.
`test_packaging_metadata.py::test_soldr_versions_move_in_lockstep` parses the
exact version out of `build-system.requires` and asserts all three declare it.
Two sites had already drifted unnoticed and are corrected here: `dylint.yml`
sat on 0.8.0, and `./install` on 0.7.11. Nothing caught either, because the
previous test compared by substring (`"0.8.0" in line`, which `"0.8.27"` does
not contain) and never looked outside `.github/workflows/`. The guard now
checks both workflows and composite actions, including the central
`setup-build` action's forwarded input default. It also rejects a non-exact
requirement outright, so reverting to a floor fails loudly rather than
silently reopening the hole, without asserting the active pin as a separate
test expectation that would become a fourth edit site.

Raising `./install` required a fix, not just a number: soldr 0.8.x stopped
publishing `.tar.gz`/`.zip` and ships `.tar.zst`, which needs a `zstd` binary
the script cannot assume — on git-bash, GNU tar 1.35 accepts `--zstd` and
then dies with `zstd: Cannot exec`. The script now prefers the release's
**wheel**, a plain zip carrying the same binary under
`soldr-<version>.data/scripts/`, extracted by the `unzip`-or-Python path it
already had; the legacy archive stays as a fallback for 0.7.x, which
published no wheels.
`test_install_script_uses_wheel_with_legacy_fallback` guards that asset
strategy, so the lockstep assertion cannot be satisfied by leaving the
installer on its legacy archive-only path.

One soldr version stays knowingly **outside** the lockstep set:
`crates/clud-bin/assets/tools/docker/docker_build_soldr.py`'s `ARG
SOLDR_VERSION`, which pins the soldr baked into the bundled Docker image and
is asserted as a literal by `crates/clud-bin/src/tools.rs`. It is bumped to
0.8.28 here, but folding it into the Python test would couple a Rust
guardrail to a packaging test for a lane that builds nothing this repo ships.
CLAUDE.md names it as an explicit exclusion so a bumper knows it exists.

Rejected alternatives: a floor-plus-patch-ceiling (`>=0.8.27,<0.8.28`) is an
exact pin with extra syntax; leaving it unbounded would only be defensible if
soldr's release gate built a downstream consumer, which it does not.

**Consequences:** clud no longer picks up soldr fixes automatically — during
this same incident the *fix* also arrived by publication, so a broken upstream
release now costs a deliberate bump PR either way. That is the trade being
bought: `main` cannot go red without a clud-side change first, and yesterday's
build is reproducible. Bumping soldr is one commit touching `pyproject.toml`,
every workflow, and `./install`; the test names each drifting site in its
failure message so none is forgotten. `dylint.yml`'s cache key embeds the
soldr version, so its first run after this change pays one cold build.

CI exercises both resolution paths. The wheel build invokes maturin directly,
but `bash test` builds and installs the local project through its PEP 517
backend after the initial dependency-only sync. The exact backend pin
therefore protects source installs and the test lanes, while the
`setup-soldr` pins protect every lane's Rust toolchain. That overlap is
exactly why the three spellings must not be allowed to drift apart.
---

## DD-021: Automatic Windows tool cleanup requires positive lifecycle roles

**Context:** zackees/clud#616. The #569 Job completion-port listener selected
tool shells by executable basename at every process depth. That recovered
genuine leaked `gh`/`git`/build clients, but it also made an ordinary nested
`cmd.exe` authoritative: when Python's `os.system("start cmd")` wrapper exited,
the intentionally detached terminal was killed. A related false-positive path
killed `conhost.exe`, which destroys the console and can leave its client
running headless. False positives are destructive, while a false negative costs
only deferred cleanup.

**Decision:** Backend authority starts with an explicit runner registration of
PID plus OS start time. The captured tree must then reach the exact agent host
(`codex.exe`, native `claude.exe`, or the npm Claude launcher's first
`node.exe`) before a direct child shell can receive the `ToolShellRoot` role.
The agent image is an authority boundary: any direct
non-shell child begins a `Client` subtree permanently, even when that child or
its descendants use familiar runtime/shell image names. Git Bash's direct
Bash-to-Bash re-exec transfers completion ownership. A nested shell beneath a
non-shell client is treated as an intentional detach boundary. Declared daemons
(`RUNNING_PROCESS_IS_DAEMON`) remain the positive escape contract for
long-lived services. `conhost.exe` is an unconditional pruned subtree and is
never terminated by automatic cleanup.

The complete role and exit decision is a pure function over captured process
metadata, registered backend identities, and the current declared-daemon set.
The Win32 completion-port listener only captures events, executes the plan, and
writes structured `reap` / `spare` / `handoff` records.

**Alternatives rejected:**

- *Keep basename matching and add more exclusions.* Depth remains ambiguous;
  every new wrapper creates another destructive special case.
- *Use the inherited CLUD originator tag as the detach signal.* Ordinary
  detached terminals and Docker helpers inherit it too, so it cannot
  distinguish intent.
- *Treat parent death as completion.* Git Bash re-exec makes a dead recorded
  parent part of healthy execution, and conhost's parent is its creator rather
  than its client.
- *Store bare backend PIDs.* Windows reuses PIDs; a stale registration could
  grant an unrelated process automatic-kill authority. Start time is part of
  the identity and a missing/mismatched identity fails closed.

**Consequences:** Automatic reaping remains prompt for direct agent tool-shell
leaks, while ambiguous nested/detached shapes survive. Some unmarked custom
daemon shapes can now be false negatives; callers that intentionally outlive a
tool must use the declared-daemon marker. Every decision is diagnosable from
the structured event log. Job PIDs whose metadata publication lags their
creation notification are retained and retried; an irrecoverable metadata miss
is logged and fails closed instead of inferring authority from the PID.
Non-Windows execution is unchanged.

## DD-022: The large-file guard reads the git index in-process, not the worktree

**Context:** zackees/clud#132's startup guard walked the whole worktree with the
`ignore` crate's parallel walker on **every** launch — ~240-400 ms locally and
~3.3 s over loopback SMB, paid synchronously before the backend starts, to
answer a question whose answer changes maybe weekly (issue #556, parent #551).
Four cheaper routes were benchmarked. Subprocess `git ls-files -z` plus a stat
per candidate lost at scale: two ~50-70 ms Windows spawns before any file work,
`--others --exclude-standard` is itself a single-threaded worktree traversal, and
`ls-files` output carries no sizes, so the git path tied or lost against the
walker on a 4416-file repo. ODB routes (`ls-files --format='%(objectsize)'`,
`ls-tree -l`) were 3-12x slower and can trigger remote fetches on partial clones.
`git ls-files --debug` was fast (52 ms) but spends ~30-50 ms on spawn+config,
emits ~6 text lines per file, and its format is documented unstable.

**Decision:** Parse `.git/index` in-process with `gix-index` (pinned exactly;
MIT OR Apache-2.0). The index already stores each tracked file's cached stat
size, so the tracked-file report needs no worktree I/O, no ODB, no hashing and
no subprocess — ~1-5 ms. `gix-index` is chosen over the `--debug` subprocess for
large-monorepo format support (index v2/v3/v4 path compression, split, sparse),
which the unstable text output cannot promise. Index discovery resolves the
`.git`-file `gitdir:` indirection so a linked worktree reports against its own
index. Entries whose cached size is untrustworthy (racily-clean, recorded as 0)
are re-measured with one targeted `stat` each rather than dropped or flagged.
The `ignore` walker is retained as the fallback for a missing or corrupt index,
so behavior never regresses; untracked-file coverage moves off the launch path
to the daemon-side pass 2 (#551), consistent with the guard being a crude fast
nudge rather than a comprehensive audit.

## DD-023: The daemon spare-list is not deleted, even though clud never sets the marker itself

**Context:** zackees/clud#673 rev 3.0, recorded so it is not re-proposed.

An earlier draft of the reaper's burn fix concluded that
`find_declared_daemon_pids()` computes an always-empty set and should simply be
deleted. The reasoning was one command:
`grep -rn "RUNNING_PROCESS_IS_DAEMON" crates/` returns zero hits, therefore
nothing in this repository ever sets the marker, therefore the set is empty and
the per-exit full-host environment scan that computes it is pure waste.

That was wrong. The marker is set by **other programs**, not by clud.
`running_process::spawn::spawn_daemon_inner` applies it to every daemon-spawn
variant, and its own comment names the consumers: *"including the free functions
consumers like **zccache** call directly."*
`spawn_daemon_breaking_away_from_job` is documented for *"a build cache server,
a language server, anything discovered and reused by later, unrelated
invocations."* On a soldr/zccache machine the set is non-empty, and its members
are exactly the processes that must never be reaped. Shipping the deletion would
have made clud kill the user's build cache.

**Decision:** The daemon spare-list stays. Its *cost* is fixed by evaluating it
over the reaper's own Job Object membership instead of the host process table,
and by caching each identity's answer — not by removing the protection. The
marker is ranked **below** every OS-authoritative signal (job-object membership,
service session, session leader, token owner) precisely because opting in is
optional: sccache, dockerd and `FBuildWorker` never call it, so a design that
treated the marker as the only line of defence would leave them unprotected.
Listening-endpoint ownership covers that population. A name-based whitelist is
the last resort and must be data, not code.

**Alternatives rejected:**

- *Delete the spare-list.* Kills the user's build cache. This is the decision
  being recorded.
- *Keep the marker as the sole signal and just make it cheaper.* Leaves sccache,
  dockerd and `FBuildWorker` with no protection at all, which is the gap
  zackees/clud#674 exists to close.
- *Ship a built-in image-name allowlist.* Misfires on every unrelated build, and
  cannot be corrected by an operator without a release.

**Consequences:** The reaper spares more than it strictly must, which is the
correct direction — a false negative costs deferred cleanup, a false positive
destroys a user's warm cache. The transferable lesson is the reason this is a
recorded decision rather than a code comment: **a runtime set populated by other
programs cannot be characterized by grepping this repository.** The caveat is
stated where an agent will meet it, in
[`architecture/process-reaping.md`](architecture/process-reaping.md).

## DD-024: Reap decisions take injected process facts rather than calling Win32 inline

**Context:** zackees/clud#673 / #674. The reaper's spare-list needs signals only
the OS can answer — Job Object membership, terminal-services session id, POSIX
session leadership, token ownership, listening-endpoint ownership. The obvious
implementation calls those APIs at the point of decision. The obvious
implementation is also untestable: the decision would then require a real Job
Object, a real detached process and a real listening socket, so every case would
be a Windows-only integration test that spawns processes and races timing.

That is not a theoretical cost. The defect zackees/clud#674 exists to fix is a
*missing test* — no coverage asserted that a long-lived daemon started inside a
clud session survives it — and the reason none existed is that writing one
required exactly that machinery.

**Decision:** Every OS-authoritative signal sits behind a `ProcessFacts` trait.
Signals are collected once per reconcile pass into a `FactsSnapshot` and then
consulted as **pure data**; no code that decides reap-or-spare calls Win32. The
Win32 collection lives in one submodule of the listener. `FactsSnapshot` is both
the production carrier and the test fixture, so there is no second implementation
that could drift from the one under test. A signal the platform cannot answer is
recorded as *unavailable* and never spares.

The rule this encodes, stated as an architectural constraint rather than a style
preference: **prefer unit tests; an integration test is justified only when the
behaviour cannot be expressed against injected facts** — in practice only when a
real Job Object or real detachment is the thing under test.

**Alternatives rejected:**

- *Call the APIs inline and cover the behaviour with integration tests.* Windows
  exec lane only, seconds per case instead of microseconds, and it forces every
  future refinement into the slow lane. This is the status quo the decision
  replaces.
- *A `#[cfg(test)]` seam or a mock crate.* Two implementations means the tested
  one can drift from the shipped one; the snapshot is one type used by both.
- *Pass individual booleans into the planner.* Loses the precedence ordering,
  which is itself behaviour worth asserting, and makes adding a signal a
  signature change at every call site.

**Consequences:** The daemon decision table — zccache, soldr, sccache,
`FBuildWorker`, dockerd, language servers — is a table-driven unit test that runs
on every platform CI builds for, asserting the *reason* a process was spared and
not merely that it was. Integration coverage shrinks to what genuinely cannot be
faked, with a stated budget of ≤5 tests. The cost is one indirection on a path
that runs at most a few times per second, and the discipline that a new signal
must be added to the trait rather than called where it is needed.

---

## DD-025: The broker frame lane is the default daemon transport, superseding DD-006's TCP-only rationale

**Context:** [DD-006](#dd-006-cluddatardb-is-owned-exclusively-by-a-single-gc-daemon-process-clients-access-it-over-loopback-tcp)
chose loopback TCP for daemon IPC and explicitly rejected "named pipes / Unix
sockets directly" as platform-specific. `running-process` adoption later added a
broker v1 frame lane, and `send_daemon_request` now tries it **first** — which
means clud's default daemon transport is a named pipe on Windows and a Unix
socket everywhere else. The rejected mechanism became the primary one, and
nothing recorded the reversal.

That is not a documentation nicety. `docs/architecture/daemon-ipc.md` cited
DD-006 as the rationale for TCP *over* named pipes while the named pipe was
carrying the traffic, and an agent reading it as authoritative reached a wrong
conclusion about what the daemon could do (#692).

**Decision:** The broker frame lane (`daemon/rp_broker/`) is the default
transport for every `DaemonRequest`. Loopback TCP remains as the fallback and is
still authoritative in the sense that it always works: any miss —
`RUNNING_PROCESS_DISABLE=1`, a missing `daemon-identity.json` sidecar, a connect
or wire failure — falls through to it silently.

**Rationale:**
- DD-006's objection was *platform-specific code we would have to write*.
  `running-process` writes it, and clud consumes one `Endpoint` abstraction, so
  the cost the original decision was avoiding is no longer clud's to pay.
- The frame lane multiplexes many frames over one connection, which loopback TCP
  as wired here does not (one request per connection). That capability is a
  precondition for any future streaming or subscription surface.
- Keeping TCP as an always-available fallback means the reversal adds a fast
  path without adding a failure mode: no sidecar, no broker, no problem.

**Consequences:**
- "The loopback TCP listener" is no longer a complete description of daemon IPC.
  There are three listeners — frame lane, TCP, and the dashboard's HTTP port.
- Connection lifetime is a **client** policy, not a protocol limit. The daemon's
  `serve_connection` already loops; the client sends one request and drops the
  session. A caller wanting a long-lived channel does not need the daemon
  rewritten.
- DD-006's alternatives table is annotated rather than rewritten: the decision
  was correct when made, and the record of why it changed is more useful than a
  silent edit.
- See [daemon-ipc.md](architecture/daemon-ipc.md) for the lane table and the
  full eleven-variant request surface.

## DD-026: Model provider and executable harness are independent launch dimensions

**Context:** zackees/clud#625. The historical `Backend` enum simultaneously
meant model API, executable, command syntax, setup target, and persisted
default. That identity fails for Codex models driven through the Claude agent
harness and makes a future protocol bridge impossible to represent honestly.

**Decision:** Resolve `ModelProvider` and requested `HarnessSelection`
independently with CLI-over-global-over-built-in precedence. `default` maps to
the provider-native harness. One `ResolvedLaunchTarget` carries both effective
values and their `PreferenceSource`; unsupported Claude-through-Codex is an
error, never a fallback. The concrete `Backend` remains temporarily as the
effective executable compatibility type at existing bootstrap, setup, runner,
and daemon call sites.

Global changes are one locked settings-document patch. A shared generic choice
state machine serves provider, harness, and session/global selectors. New
`LaunchPlan` metadata is optional under serde so old daemon payloads fall back
to their legacy `backend`; repeat jobs pin their choices in argv.

**Consequences:** Existing invocations remain native and unchanged, while
`clud --codex --harness claude` is represented without lying about either
dimension. Later bridge phases can attach transport/auth behavior to the
resolved route rather than adding more backend booleans.

## DD-027: The Codex-to-Claude bridge is a launch-scoped bounded HTTP shell

**Context:** zackees/clud#626. A Codex model can be selected while the Claude
executable remains the effective harness, but that child expects an Anthropic
HTTP endpoint and bearer token. The compatibility route therefore needs a
local transport owner before later phases add request translation.

**Decision:** A single foreground runtime owns an authenticated bridge for the
whole launch and overlays only the spawned child's environment. The bridge
binds an ephemeral IPv4 loopback port, generates a per-launch 256-bit bearer
token, implements the minimal `/v1/messages` fixture surface, and shuts down
with the child on every return path. Native routes and dry-run do not start it.

The listener uses a small standard-library HTTP shell instead of `tiny_http`.
`tiny_http` starts connection parsing before application code receives a
request, so clud cannot enforce the required header-byte cap and header read
timeout at the transport boundary. The local shell keeps those controls, the
body cap, and the worker concurrency bound in one auditable layer.

**Consequences:** The bridge is intentionally not a daemon.

> **Partly superseded by [DD-029](#dd-029-the-bridge-always-streams-upstream-and-status-is-chosen-only-before-the-first-frame).**
> The paragraph below described phase 2, where the bridge did not yet translate
> or forward production traffic and answered from compiled fixtures. Since
> #627 step 5 it runs the real pipeline; the fixtures are gone. The transport
> and security decisions above still stand.

Its deterministic non-streaming and
SSE responses exist for compiled-fixture validation. Any debug upstream seam
is gated by both a debug build and `CLUD_INTEGRATION_TESTS=1`; release builds
ignore it. Logs and fixture reports expose only sanitized presence, port, and
status metadata, never the token or full authenticated URL.

## DD-028: The bridge times each I/O phase separately and streams responses chunked

**Context:** zackees/clud#627 phase 3 step 1. Phase 2's bridge carried one
`io_timeout` used as a whole-connection deadline, and a single `write_response`
that derives `Content-Length` from a fully materialised body. Both are correct
for a fixture server and wrong for model traffic: a real streamed completion
routinely outlives any deadline short enough to be a useful slowloris defence,
and a response whose length must be known up front cannot be delivered
progressively. The child is already configured for long turns —
`apply_cross_route_overlay` sets `API_TIMEOUT_MS` to 3 000 000.

**Decision:** Split the single budget into `header_timeout` (5 s),
`body_timeout` (30 s), and `stream_idle_timeout` (300 s). The first two are
absolute per-phase deadlines; the third is an *idle* timeout re-armed before
every frame, so total response duration is unbounded while a peer that stops
reading is still cut off. Add `write_event_stream` alongside `write_response`:
chunked transfer encoding, one chunk and one flush per SSE event. Errors and
non-streaming replies keep the original writer, which still owns the only path
that can choose a status code.

Reads are performed on blocking sockets with the per-read timeout capped at
`READ_POLL`, and a timeout is only fatal once the phase deadline has actually
passed. The cap exists so a worker parked on a quiet socket observes the
shutdown flag promptly; before this, a blocked read held teardown for its full
budget, because `shutdown()` on another thread does not reliably interrupt a
blocking `recv` on Windows.

**Consequences:** A body arriving in a different TCP segment from its headers
is now read correctly. It previously could not be: `TcpListener::set_nonblocking`
is inherited by accepted sockets on Windows, a non-blocking socket ignores
`SO_RCVTIMEO`, and the readers classified the resulting `WouldBlock` as a
timeout — so any request whose body did not arrive with its headers was
answered `408` immediately. Phase 2's tests never saw it because they write
headers and body in one call; every real Claude request carrying a transcript
or an image spans segments. Accepted sockets are now explicitly returned to
blocking mode, which also keeps the retry loop from busy-spinning.

Phase 3's translator replaces the fixture frames but inherits this framing
contract: one vector element per complete SSE event, flushed as produced.

**Amendment (#1263):** the *upstream* hop of the Anthropic passthrough proxy
did not follow the paragraph above. It reached for `ureq`'s
`Request::timeout(stream_idle_timeout)`, which ureq documents as covering
"reading the response body" and which *takes precedence over*
`AgentBuilder::timeout_read()` — so the budget was a whole-request deadline
wearing an idle timeout's name. Two things followed, both wrong in the same
direction: a stream that simply ran longer than 300 s (ordinary for a model
that thinks for minutes and keeps emitting deltas) was cut off mid-turn, and a
socket that had gone quiet was only noticed once that same absolute wall-clock
had elapsed. The hop now reads through an agent configured with
`timeout_connect` / `timeout_read` / `timeout_write`, which is the shape the
sibling `codex_upstream` hop has always used and the shape this entry claims.

Two limits are worth stating plainly, because neither is a bug to be fixed
later:

- **A committed stream terminates; it does not retry.** The status line is sent
  with the first frame (DD-029), so once a frame is on the wire a mid-stream
  failure has no status left to choose and no request left to replay — the
  transcript is half-delivered and replaying it would duplicate it. The only
  honest report is in-band, and the proxy now emits the same sanitized SSE
  `error` frame the translator does instead of writing the terminating chunk
  and closing, which was byte-identical to a stream that ended because the
  model finished.
- **Idle is measured on received bytes, never on turn duration.** A long think
  is healthy and a dead socket is not, and wall-clock alone cannot tell them
  apart. `stream_idle_timeout` is therefore correct at 300 s for this hop: a
  model may legitimately emit nothing for minutes *before its first token*,
  which is why the sibling hop keeps a separate `first_frame_timeout` for the
  pre-commit gate. Lowering the idle budget to catch a stall sooner would kill
  legitimate long turns first.

## DD-029: The bridge always streams upstream, and status is chosen only before the first frame

**Context:** zackees/clud#627 step 5 wired the translator, upstream client, and
SSE state machine into the bridge's `POST /v1/messages` handler, replacing the
phase-2 fixtures. Two shapes had to be served — Anthropic's streaming and
non-streaming replies — and failures can occur either before or after output
has started.

**Decision:** Send `stream: true` upstream unconditionally. A non-streaming
Messages request is answered by folding the translated Anthropic events back
into one `Message` with `MessageAggregator`. The alternative, a second
request/response mapping for the non-streaming shape, would double the surface
that has to stay correct while reusing none of the fuzzing that step 3 spent on
the streaming path.

Downstream status is chosen only while nothing has been written.
`EventStreamWriter` therefore defers its HTTP headers until the first frame:
before that a failure is a real status (`400` malformed, `422` unrepresentable,
`401` invalid local bearer, `502` unavailable/rejected upstream credentials,
`503` temporary refresh failure, other upstream `4xx` generally passed through,
`502`/`504` otherwise);
after it the response is committed and a failure is reported in-band as a
sanitized SSE `error` event, with the chunked body terminated cleanly. This is
the same boundary the upstream retry policy uses, and for the same reason.

**Consequences:** Aggregation is a pure function of the event stream, so the
non-streaming path inherits every property the streaming path proves. The
handler never has to decide whether it is "too late" to fail — it asks the
writer. Upstream bodies are never propagated in either direction, since they
carry account identifiers and key fragments.

The debug seam now points at a Responses-shaped fake rather than phase 2's
passthrough. That passthrough echoed the Anthropic body, so the end-to-end
tests could pass while translation was entirely wrong; the integration tests
now assert on the request the fake receives.

## DD-030: The bridge conforms to the live Codex clients, and translation is total

**Context:** zackees/clud#750. Phase 3 built the translator from the *shape* of
the Anthropic and OpenAI APIs. An audit against CLIProxyAPI (MIT) and
`openai/codex` (Apache-2.0) — the two implementations #622 names as references —
found it diverging from both in ways that break real traffic.

**Decision:** Match observable behaviour of the live clients, not a reading of
the API surface. Concretely:

- **Translation is total.** #627 made "unsupported semantics fail explicitly" an
  acceptance criterion; this reverses it. CLIProxyAPI's translator never errors,
  and Claude Code really does send `top_k`, `stop_sequences`, replayed
  `thinking` blocks and `role: "system"` messages. A 4xx the bridge invents is a
  failure the user cannot act on, so those inputs are dropped or adapted.
  `Invalid` — a request that is not a Messages request at all — is the only
  remaining error.
- **Sampling parameters are never forwarded.** Neither reference sends
  `temperature`, `top_p` or `max_output_tokens`, and reasoning models reject
  them.
- **`store: false` plus `include: ["reasoning.encrypted_content"]`** are sent
  unconditionally, as both references do. They are load-bearing together: with
  no server-side state, reasoning has to round-trip.
- **Reasoning round-trips.** An Anthropic `thinking` block's `signature` *is*
  the reasoning item's `encrypted_content`. Phase 3 dropped reasoning believing
  no signature was available; that premise was wrong. Foreign or malformed
  signatures are dropped rather than replayed, because replaying one is a hard
  upstream error.
- **System-prompt placement depends on auth mode.** `openai/codex` uses
  `instructions`; CLIProxyAPI uses a `developer` message because the Codex
  backend expects `instructions` to be Codex's *own* prompt. Modelled as
  `SystemPlacement` and selected from the resolved target.
- **Identifiers are bounded and reversible.** `call_id` and tool names are
  shortened to 64 characters with a hash suffix and a per-request reverse map,
  so MCP tool names survive the round trip with the names the client sent.

**Consequences:** The default model stays a single overridable value. Codex
fetches its catalogue from the server, so a hardcoded table would rot — and one
already would have: `gpt-5.4` retires from ChatGPT-auth Codex on 2026-08-31.

Validated against a real ChatGPT subscription: a streamed text turn and a
tool-use round trip both complete end to end. That validation surfaced a routing
defect no mock could: Claude Code sends `POST /v1/messages?beta=true`, and the
bridge matched the raw request target, so every real request 404'd. The mock
probe sends a bare path and had never exercised it.

**Amendment (#1220):** A usable Codex CLI login may be copied into clud's
separate credential store only after an interactive foreground bridge prompt.
The prompt is the explicit choice required by this decision; non-interactive,
daemon, expired, corrupt, and unreadable clud-record paths retain their prior
failure rather than switching credential sources. `never` records a refusal
and `always` records an opt-in under `codex.import_cli_login`; neither changes
the Codex CLI's files, and `clud auth logout codex` still removes only clud's
copy. The import prompt warns that the two stores are independent: a future
refresh may rotate the copied refresh token, so the Codex CLI can require its
own re-login. clud deliberately does not write another application's auth
file.

**Amendment (#1274):** Clud refreshes its selected subscription record on use
near expiry, with one guarded pre-output refresh/retry after an upstream 401.
After visible output, an in-band auth failure only marks the next turn for
credential recheck; it cannot replay the committed turn. A rejected refresh
grant may prompt an explicit same-account repair or account switch only in an
interactive foreground launch. Neither `always` nor another account silently
replaces the selected record. A downstream 401 remains reserved for a bad local
bridge bearer; upstream auth failures are gateway failures, not evidence that
Claude's local bearer is invalid.

## DD-031: Git-Bash completions are suppressed in the backend's login shell

**Context:** zackees/clud#753. On Windows every Claude Code `Bash` tool call was
costing ~4.4 s of CPU on an idle machine — ~20 s once the resulting process
storm saturated the box — before running any of the actual command.

The cause is a three-way collision, none of whose parts is wrong alone. Claude
Code builds a per-session "shell snapshot" by running the shell as a **login**
shell (`execFile(shell, ["-c", "-l", script])`) and replaying the captured
functions into every later tool call. Its capture filter,
`declare -F | cut -d' ' -f3 | grep -vE '^_[^_]'`, drops single-underscore
completion functions but *deliberately* keeps double-underscore helpers so
things like mise's `__zsh_like_cd` survive. On Windows, `-l` pulls in
`/etc/profile.d/git-prompt.sh`, which sources `git-completion.bash` — and Git's
completion internals are all `__git_*`, so ~84 of them pass the filter. Each is
serialised as ``eval "$(echo '<base64>' | base64 -d)"``: a subshell plus a real
`base64.exe`, i.e. **two process spawns per function, ~170 per tool call**,
under MSYS2's emulated `fork()`.

It is self-reinforcing rather than a fixed cost — more overhead means more
concurrent `bash.exe`, which means more contention, which means more overhead.
That is why the measured figure ranges from 4.4 s to ~20 s depending on load.

**Decision:** Export `WINELOADERNOEXEC=1` into the backend agent's environment
on Windows. Git for Windows guards both completion-sourcing blocks in
`/etc/profile.d/git-prompt.sh` with `test -z "$WINELOADERNOEXEC"`, so the login
shell skips `git-completion.bash` entirely. Measured: 85 captured functions → 1,
and 4,413 ms → 49 ms per tool call.

The policy lives in `shell::completion_guard::env_overrides()` and is applied by
**both** child-env builders — `runner::child_env` and
`daemon::io_helpers::child_env`. Those two are long-standing duplicates and the
daemon one had already drifted (it misses `CLUD_DISABLE_POWERSHELL` and the
Codex bridge overlay); wiring only the runner would have left daemon-launched
sessions paying the full tax. `CLUD_GIT_BASH_COMPLETIONS=1` opts back in.

**Why not the alternatives:**

- **`~/.config/git/git-prompt.sh`** — `git-prompt.sh:8` short-circuits the whole
  block if that file exists, which is cleaner and officially supported. But it
  is a *user-global* file that also changes the user's interactive shells. clud
  must not write it silently.
- **`CLAUDE_CODE_DONT_INHERIT_ENV`** — exists in the binary but only governs
  whether `process.env` is inherited. The functions come from `/etc/profile.d`,
  which a login shell reads regardless.
- **Fixing it in Claude Code** — the actual fix, and it is three lines: append
  `declare -f "$func"` straight to the snapshot instead of round-tripping
  through base64. Claude Code's own *zsh* branch already does exactly this
  (`typeset -f`); only the bash branch takes the detour. That costs zero spawns
  regardless of how many functions are captured, on every platform. Filed
  upstream; this DD covers the mitigation we control.

**Consequences:** This is a mitigation scoped to the worst-case platform. A
Linux or macOS user with a function-heavy `.bashrc` still pays the full
round-trip, because the lever is Git-for-Windows-specific.

`WINELOADERNOEXEC` is a variable Git for Windows *consults*, not one it
documents as an API, so a change on their side would silently stop suppressing
completions and the tax would quietly return. The guardrail is therefore
`tests/cli/shell_completion_guard.rs`, which asserts the observed **function count**
of a real login shell rather than merely that the variable is set — an
env-var-presence assertion would keep passing through exactly the regression it
is meant to catch. The variable is deliberately not set off Windows, where Wine
may genuinely be running.

Side-effect surface was verified as a single line: diffing the full exported
environment of a login shell with and without it set shows only `PS1` losing its
`` `__git_ps1` `` segment, which is meaningless in a non-interactive tool-call
shell. PATH, every other exported variable, aliases and `git` itself are
identical.

## DD-032: The bridge classifies an upstream failure before deciding to retry it

**Decision:** `UpstreamClient` reads the error response — a bounded body prefix
plus `cf-ray`, `x-request-id` and `Retry-After` — reduces it to a classified
`UpstreamFailure`, and lets that class pick the retry budget. `502` is no longer
the catch-all downstream status for every non-gateway failure.

**Context:** The previous code discarded the response entirely
(`Err(ureq::Error::Status(status, _))`), keeping only the integer, and mapped
five unrelated failures — a real upstream 5xx, a transport reset, an oversized
response, a cancelled request, a downstream hangup — onto a single `502` whose
client message was the generic `"upstream provider error"`. An operator seeing a
502 could not tell a Cloudflare edge blip from a hard rejection from a bug in
clud itself, and neither could the retry loop.

That mattered because retrying is not always safe. Upstream returns *permanent*
rejections wearing a 5xx costume: a model that requires a newer client, an
unsupported parameter. Retrying those can never succeed, and CLIProxyAPI#4327
documents where it ends — one request fanning out to N upstream attempts, a
burst from a single exit IP tripping a Cloudflare 520, and healthy credentials
driven into cooldown. The old policy retried every `>= 500` identically, and its
total retry window was ~0.75s, which is simultaneously too eager for the
permanent class and far too short for the transient one.

**Alternatives rejected:**

- *Just raise `max_attempts`.* This is the change that produces the cascade
  above. Widening retry is only safe once the permanent class can be excluded,
  which is why classification is the load-bearing half and the budget increase
  rides behind it.
- *Retain the raw body on the error.* Rejected: upstream bodies can carry
  account identifiers and key fragments, and #630 makes not propagating them a
  hard rule. The body is read, classified, mined for a scrubbed one-line
  `detail`, and dropped. Only the class, the opaque ids and that scrubbed detail
  survive, and only the ids reach the client.
- *Fold `Unknown` into `Transient`.* Rejected for the same cascade reason.
  Folding it into `Permanent` was also rejected: it would break the first time
  upstream introduces a legitimately new transient code. A reduced budget is the
  safe middle, and it is the case that stays quiet when we guess wrong.
- *Classify on status alone.* Insufficient by construction — the whole problem
  is that the same status carries both classes. sub2api#4020 is the worked
  example: `gpt-5.6-sol` refused with a version-gate message inside a `502`.

**Consequences:** Classification is substring matching over a lowercased body,
so it is heuristic and will mis-file novel messages. The blast radius of a wrong
guess is bounded on purpose: a mis-filed permanent failure costs one extra
attempt (the `Unknown` budget), and a mis-filed transient one fails a request
that would likely have failed anyway. The signature lists are the thing to
extend when a new shape shows up, not the control flow.

The no-replay-after-first-byte invariant (DD-029) is untouched and still tested;
every change here is confined to the pre-commit window, which is the only place
a `502` could ever have been chosen.

`499` is not an RFC status. It is the conventional code for "client closed
request" and unambiguous in a log, and by the time it is selected there is
normally no reader left to receive it.

The expired-login guardrail reads a JWT `exp` claim **without verifying the
signature** — clud has no key, and verification is the issuer's job. It exists
only to avoid starting a turn on a token that is already dead, so a bearer that
is not a JWT, or carries no `exp`, is deliberately treated as live: opaque
tokens are legitimate. Actual refresh remains #629's scope.
---

## DD-033: Plan mode and subagents are constrained on the Codex-to-Claude bridge

**Status:** Accepted

**Context:** On `clud --codex --harness claude`, users reported the agent
entering plan mode with no prompting — an ordinary question ("can we always
enable that debug log?") turning into an unrequested planning session with
three exploration subagents.

This is not a clud defect and not a bridge translation bug. Plan mode has two
entry paths in the Claude harness: the user toggles it (shift+tab), or **the
model enters it itself** by calling the harness-provided `EnterPlanMode` tool.
That tool's own description instructs the model to use it *proactively* for
non-trivial implementation asks, listing new features, multiple valid
approaches, architectural decisions, multi-file changes and unclear
requirements as triggers. A feature-shaped question hits several at once, so
the model volunteers a plan. `--dangerously-skip-permissions` does not cover
this; only `--disallowedTools` removes the tool.

clud already stripped `EnterPlanMode,AskUserQuestion`, but only when
`is_unattended` (a `clud loop`, or explicit `--unattended`). The reported
sessions were **interactive**, so the flag was never emitted.

**Decision:** Disallow `EnterPlanMode` on every launch where the model provider
is Codex and the effective harness is Claude, independent of `--unattended`.
Also disallow Claude Code's `Task` tool on that bridge: the Task tool creates
background Claude agents, each with an independent provider request, so it can
turn one requested harness run into unbounded subscription spend. `--allow-plan-mode`
opts back into planning only; it never restores `Task`. When plan suppression
applies, clud prints a green, stderr, TTY-only notice naming the override, so
the behavior is never silent.

**Alternatives rejected:**

- **Extend the rule to all Claude-harness launches.** Simplest diff — delete
  `is_unattended &&`. Rejected: on a plain `clud`, plan mode is a feature users
  deliberately reach for, and a global kill would take shift+tab away from
  people who never asked. The complaint is specific to the bridge, where a
  Codex model is driving Claude-harness tooling it was not tuned against.
- **Suppress `AskUserQuestion` too, matching the unattended token.** Rejected:
  multiple-choice questions are useful interactively and were not part of the
  complaint. The unattended rule keeps stripping both, because a run with no
  human attached stalls on either one; the bridge rule is narrower on purpose.
- **A silent suppression.** Rejected: removing a harness capability without
  saying so produces the mirror-image confusion — "why can I not plan?" The
  notice costs one line and carries the override.

**Consequences:** The two rules now compose into one `--disallowedTools` token
rather than a fixed string, and `--allow-plan-mode` deliberately does **not**
re-enable plan mode for `--unattended` / `clud loop` runs on the bridge; the
older stall-avoidance reason still applies there and is asserted by
`test_allow_plan_mode_does_not_re_enable_it_for_unattended_runs`.

The token stays `=`-bound and comma-separated for the reason in DD-002's
neighborhood and `builder.rs`: `claude` declares `--disallowedTools` as
variadic, so a space-separated spelling swallows a following `-p <prompt>` and
claude exits 0 with no output and no diagnostic.

## DD-034: The bridge's default model is the cheap tier, not the flagship

**Status:** Accepted

**Context:** zackees/clud#776. `DEFAULT_CODEX_MODEL` was `gpt-5.6-sol`, the
flagship of the gpt-5.6 family, and nothing could override it at runtime:
`resolve_model` (`codex_translate.rs`) only forwards a request's own model when
the id does *not* start with `claude`, and the Claude harness always sends
`claude-*`, so every bridged request fell through to the constant. The two
override seams that exist — `UpstreamTarget::with_model_override` and
`Pipeline::with_default_model` — had no production callers.

The family is three tiers at one context size (1,050,000 tokens), differing
only in price and default effort:

| id | tier | $/1M in | $/1M out | catalog default effort |
| --- | --- | --- | --- | --- |
| `gpt-5.6-sol` | flagship | $5 | $30 | `low` |
| `gpt-5.6-terra` | mid | $2 | $12 | `medium` |
| `gpt-5.6-luna` | fast/cheap | $0.2 | $1.2 | `medium` |

A default nobody selected was billing at 2.5x the mid tier on both input and
output, and it drained a real credit account before anyone noticed — the more
so because the resulting out-of-credits 429 was itself swallowed (#774).

**Decision:** `DEFAULT_CODEX_MODEL` is `gpt-5.6-terra`. Effort is unchanged:
`reasoning_for` already emits `medium` when the request carries no `thinking`
block, and `medium` is terra's own catalog default — so the cheap tier is also
the correctly-configured one, and "terra at medium" needs no effort change.

The default is asserted on the **wire**, not against the constant
(`codex_pipeline.rs::the_billed_default_is_terra_at_medium`). A test written as
`assert_eq!(sent["model"], DEFAULT_CODEX_MODEL)` follows the constant wherever
it goes and by construction cannot notice a change in what the user is charged;
three such assertions existed and all three stayed green across the flip.

**Alternatives rejected:**

- **`luna`.** 10x cheaper again, but it is the fast tier and wrong for a main
  coding loop. It belongs in the alias table as an explicit opt-in.
- **Keep `sol` and add an override first.** Rejected on sequencing, not merit:
  the override work (#752) carries an open question about whether Claude Code
  offers effort controls for a non-`claude` model id, and the cost bleed should
  not wait on it. The flip is one string; the selection feature lands after.
- **Read the model from an env var here.** Rejected as scope: an escape hatch
  needs a settings-persistence story (`GlobalLaunchPreferences`) to be worth
  having, which is #752's territory.

**Consequences:** `codex_upstream.rs`'s version-gate regression fixture still
names `gpt-5.6-sol` deliberately — it asserts against a real upstream error
message that happens to mention that id, and renaming it would weaken the
regression it guards.

**Amendment (2026-09-21, #1254):** The reviewed direct Codex default is now
`gpt-5.6-sol` at its catalog-native `low` effort. Model/effort selection,
source provenance, cache-health protection, and live usage visibility landed
after #776, so the prior unobservable hardcoded-Sol failure mode no longer
describes the launch path. The catalog remains the single authority and feeds
both native Codex and Codex-through-Claude; explicit CLI and saved provider
profiles still win, unified mode remains harness-owned, and the Claude
overlay's separate Opus-to-Sol / Sonnet-to-Terra delegation aliases are
unchanged. Wire-level fake-upstream tests pin both the billed model and effort
so this policy cannot drift silently.

## DD-035: Codex model and effort travel in the model string, not beside it

**Status:** Accepted

**Context:** zackees/clud#752. DD-034 made the default cheap; this is how a
user picks something else. The Claude harness talks to the bridge over the
Anthropic Messages API, which has no field for "which Codex model" or "at what
effort". Two channels could carry that intent, and they are not equally
reliable:

- **`output_config.effort`** — where `/effort`, `--effort`,
  `CLAUDE_CODE_EFFORT_LEVEL` and the `/model` effort slider land. The harness
  only sends it when it decides the model *supports* effort, which it decides
  by matching the model id against known families. A raw `gpt-5.6-*` id matches
  nothing.
- **The model id itself** — never validated, never rewritten, never dropped
  behind a custom `ANTHROPIC_BASE_URL`, because the gateway is declared to own
  the model namespace. Whatever the user types in `/model` arrives verbatim.

The bridge modelled neither. `MessagesRequest` had no `output_config` field
and unknown fields are deliberately tolerated, so the user's effort choice was
**dropped without a trace**: every request ran at the ladder's `medium`
regardless of what was selected. `/effort xhigh` was a silent no-op.

**Decision:** Selection is spelled `<model>[@<effort>]` in the model string —
`terra`, `sol@max`, `gpt-5.6-luna@low` — parsed by `codex_model.rs`.
`output_config.effort` is *also* read now, as a secondary channel.

Precedence, most explicit first: `@effort` suffix → `output_config.effort` →
the `thinking` budget ladder → the model's own catalog default.

Three supporting rules:

- **An unknown short name is a 400 that names the valid ones; an unknown full
  id passes through.** `/model tera` is a typo, and forwarding it would either
  earn a confusing upstream error or — worse — silently bill a model nobody
  chose. But `gpt-5.7-whatever` is how a user reaches a model released after
  this table was written. The split is punctuation: a bare word must be in the
  table, anything containing `-` or `.` is a full id.
- **"No effort specified" is not `medium`.** Each model has its own catalog
  default (`sol` = `low`, `terra`/`luna` = `medium`), and the harness sends
  `thinking: {"type":"adaptive"}` with *no* budget for ids it does not
  recognize — which is every id the bridge serves. Reading a missing budget as
  an explicit `medium` pinned every request to `medium`.
- **The ladder no longer emits `minimal` and can now reach `max`.** `minimal`
  is a real Responses value that **no gpt-5.6 model accepts** (the family
  starts at `low`), so every small-budget request was being rejected upstream
  for a reason the user could not see. `max` is supported by all three and was
  unreachable from any budget. Re-confirmed against OpenAI's model guidance
  for #821: gpt-5.6 accepts `none`, `low`, `medium`, `high`, `xhigh`, `max`.
- **A stated-but-unsupported `output_config.effort` is a 400, not a
  fallthrough (#821).** It originally deferred to the next channel, on the
  theory that the harness's own field should never fail a turn the user is
  waiting on. But the channel below it is the model's *default*, so the turn
  ran at an effort the user never chose and could not observe — `/effort
  minimal` silently became terra's `medium`. Since `minimal` is a genuine
  Responses value, it is exactly the spelling a user would expect to work,
  which makes the silence worst here. Both effort channels now produce the
  same actionable message naming the accepted values. An *absent* or empty
  effort still defers: that is the harness declining to send the field, not a
  user naming a value.

**Alternatives rejected:**

- **`output_config` alone.** The obvious reading of the problem ("model the
  field you forgot"), and insufficient: it depends on the harness's capability
  matching offering the control for a non-`claude` id at all, which is exactly
  the thing we cannot rely on. It is kept as the secondary channel because
  users who *do* get the native control should have it work.
- **Gateway model discovery (`GET /v1/models`) to populate the picker.**
  Blocked three ways: the bridge does not serve the route, clud forces
  `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` (discovery does not run when
  nonessential traffic is off), and discovery *ignores every id not prefixed
  `claude`/`anthropic`* — i.e. every id we would advertise. Making that work
  needs synthetic `claude-codex-*` ids mapped back through the alias table, and
  it would invert `resolve_selection`'s `claude*` rule. Deferred;
  `ANTHROPIC_CUSTOM_MODEL_OPTION` (documented to skip validation) gives one
  honest picker row today for a fraction of the work.
- **Patching the harness binary** to bake in aliases, as `@bman654/clodex`
  does. It confirms the constraint is real, but an unmaintainable coupling to
  someone else's build.
- **Tier hijacking** (`ANTHROPIC_DEFAULT_OPUS_MODEL` → a Codex id), which most
  of the proxy ecosystem does. Cheap, but it lies about which model is running
  and burns the `opus`/`sonnet`/`haiku` names.

**Consequences:** `--model` on the bridge is expanded to the wire id in argv
and recorded on `LaunchPlan::codex_model`, so `--dry-run` shows what will be
billed rather than the shorthand that was typed, and every launch path —
subprocess, PTY, daemon, detach, repeat — hands the bridge the same value. A
selection that does not parse fails the *launch* rather than the first turn.

The two previously-dead override seams (`UpstreamTarget::with_model_override`,
`Pipeline::with_default_model`) now have production callers and carry a
`ModelSpec` rather than a `String`, so a model and its effort cannot drift
apart in transit.

## DD-036: The bridge propagates the classification, not the status

**Status:** Accepted

**Context:** zackees/clud#774. A real out-of-credits condition reached the user
as `upstream provider returned status 429` — naming no cause, no account, no
reset, and no remedy. The session then retried into the wall and went quiet;
the account was discovered to be empty hours later, from a different tool's
output.

Upstream had told us everything needed. #764 fixed the first half by capturing
the failure instead of binding the response to `_`, so `UpstreamFailure`
already carried the status, `Retry-After`, `resets_in_seconds`, `cf-ray` and
`x-request-id`. What remained was that **every consumer downstream of that
capture re-derived its answer from the status code**, which had already lost
the distinction the classifier computed:

- `anthropic_error_type(status)` mapped `429 -> rate_limit_error`, so
  `billing_error` could not reach the client on the non-streaming path.
- `complete()` replaced any in-band failure with
  `Transport("upstream stream failed")` — a semantic quota failure became a
  transport failure, became a `502 api_error`.
- `StreamTranslator::fail` received the full upstream error object and used it
  only to pick one of four type constants, hardcoding the message to
  `"upstream provider error"`. The provider's own wording was not redacted; it
  was never read.
- The streaming path returned HTTP 200 for a failure delivered inside a 200
  SSE stream, with **no log line even under `CLUD_CODEX_BRIDGE_DEBUG=1`**.
- `stream_json::render_line` dropped `{"type":"error"}` through its catch-all,
  so the entire user-visible trace of a failed turn was `[claude] error`.

**Decision:** The classification travels; the status is derived from it, never
the reverse.

- `FailureClass::Exhausted` splits out of `Permanent`. It is checked **before**
  the "408/429 are transient" rule, because the ChatGPT backend reports plan
  exhaustion as a 429 and status alone cannot distinguish it from a throttle.
  It earns exactly one attempt: a multi-day exhaustion previously burned three
  attempts in ~750 ms.
- `PipelineError::Provider` carries an in-band failure with its classified
  kind, so a quota failure inside a 200 keeps its identity instead of being
  relabelled transport.
- `error_type_for` consults `failure_class()` first, so `billing_error` reaches
  the client on both paths.
- The bridge emits `Retry-After` (its first response header beyond the fixed
  four) and gained the missing `429 Too Many Requests` reason phrase.
- Durations are rendered as a clock — `5d 2h`, not `442242s`.
- A terminal account failure prints one ungated, belled `[clud]` stderr line
  per process, following `wedge_watchdog`'s warn-once-per-episode precedent.
  Every other bridge diagnostic is either debug-gated or in a forensic log
  nothing reads back, and a drained account is not a debugging detail.

**The secrecy invariant is unchanged (#630).** No upstream byte reaches the
client. The bug was never that the body was redacted — it was that the body was
*deleted unread*, so no failure could be reported as what it was. Every
client-facing string here is one we wrote, selected by a classification derived
from the body and then discarded.

**Alternatives rejected:**

- **Widen `UpstreamError::Status(u16)` to carry the body.** Would put an
  upstream-controlled string one careless `format!` away from the client.
  Typed, non-secret fields make the leak impossible rather than unlikely.
- **Echo the provider's `message`.** It is the only place the words "out of
  credits" appear upstream, and it is also where account identifiers and key
  fragments appear. Synthesizing from the classification gets the same
  information across with none of the risk.
- **Treat every 429 as non-retryable.** Simpler, and wrong: an ordinary
  throttle is exactly the case retry-with-backoff exists for. The body is what
  separates them.

## DD-037: Embed the Codex-via-Claude bridge and make credentials explicit

**Status:** Accepted

**Context:** The supported cross-route must translate an Anthropic-compatible
local harness request to the OpenAI Responses API without making installation,
runtime ownership, or a credential fallback ambiguous. A Go/Node proxy or
downloaded sidecar would add an executable, updater, separate crash surface,
and target-runner runtime dependency to a feature that ships inside clud's
existing Rust artifact.

**Decision:** The bridge is an in-process Rust subsystem, bound per launch to
an authenticated ephemeral loopback address and owned by `ForegroundRuntime`.
It performs the protocol translation itself and downloads no external runtime.
Credentials are an explicit choice: a clud-owned subscription record wins when
present; only its absence permits `OPENAI_API_KEY`; expiry/error never silently
changes source. The bridge's environment overlay is child-local and native
launches do not enter this path.

**Consequences:** Release artifacts remain self-contained and the lifetime is
one owner/one shutdown path. The stricter credential rule can require a user to
log in again instead of continuing with a usable API key, but it prevents a
surprising billing/authentication source change. Rollback remains a single
`--harness default` launch or settings reset.

**Alternatives rejected:**

- **Go/Node/npm sidecar or downloaded proxy:** adds a runtime and packaging
  matrix the shipped artifact cannot guarantee.
- **A shared daemon-owned bridge:** makes listener/bearer lifetime cross
  sessions and obscures shutdown/credential ownership.
- **Fallback from an expired subscription to an API key:** may silently change
  account, billing, and policy for the same user action.
- **Fail the turn on an in-band failure inside a committed 200.** DD-029's
  no-status-after-first-byte invariant stands. The status cannot change, so the
  fix is to make the failure *visible* — log, banner, and a named SSE error
  frame — not to pretend the status is still choosable.

## DD-038: The Codex picker gets one honest row, always, carrying the catalog

**Status:** Accepted

**Context:** zackees/clud#820 asked for Sol, Terra, and Luna as three
independently selectable entries in Claude Code's `/model` picker. DD-035 had
already delivered `<alias>@<effort>` selection and a *single*
`ANTHROPIC_CUSTOM_MODEL_OPTION` row, explicitly deferring the full list.

The premise was checked against Claude Code 2.1.212's own picker builder rather
than assumed. Six sources contribute rows, and none yields three honest Codex
entries:

1. **The built-in Anthropic lineup**, renameable via
   `ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU,FABLE}_MODEL` — the tier hijacking
   DD-035 rejected, because it burns the Anthropic names and lies about what
   is running.
2. **`ANTHROPIC_CUSTOM_MODEL_OPTION`** — read once, as a scalar, and pushed as
   one `{value, label, description}`. The binary contains exactly four names
   in this family (`…_OPTION`, `…_OPTION_NAME`, `…_OPTION_DESCRIPTION`,
   `…_OPTION_SUPPORTED_CAPABILITIES`), all scalars. There is no indexed,
   repeated, or delimited form, so it cannot be made to emit a second row.
3. **Gateway discovery (`GET /v1/models`)** — needs an opt-in
   `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY`, is skipped while
   non-essential traffic is disabled (clud forces that off), and filters the
   response with `/^(claude|anthropic)/i`, dropping every id the bridge
   serves. Ruled out by #820 and by DD-035.
4. **`additionalModelOptionsCache`** in the user's global config — a cache of
   Anthropic's own server response, refreshed behind our back. Not an
   extension point.
5. **The `availableModels` settings allowlist** — an allowlist, and it only
   ever *adds* ids matching `anthropic.…` or `claude-…`; `gpt-5.6-*` is
   skipped.
6. **The currently selected model**, which is a row because it is selected —
   not a way to advertise one that is not.

**Decision:** One row is the honest ceiling, so make that row do all the work.
`codex_model::picker_entry` owns the row and is rendered from `CODEX_MODELS`
and `Effort::ALL`, so it can never advertise a model or an effort
`ModelSpec::parse` would then reject. Two behaviours follow:

- **The description carries the catalog.** It names every alias, wire id, and
  per-model default effort, plus the effort ladder — because there is no
  second row to put them in, and `/model <id>` accepts any string once a
  custom `ANTHROPIC_BASE_URL` owns the namespace, so naming them is enough to
  make them reachable.
- **The row is unconditional.** It previously appeared only with an explicit
  `--model`; an unpinned `clud --codex` therefore showed a picker of
  Anthropic names, *all* of which quietly ran on `gpt-5.6-terra`. The row now
  always exists and spells the model that will actually be billed.

**Consequences:** The picker is honest in the default case for the first time,
and the two models a user did not launch with are discoverable from inside the
picker instead of only from `--help`. `ANTHROPIC_CUSTOM_MODEL_OPTION` only adds
a row — it does not change the active model — so an unpinned launch still
resolves `claude*` ids to the bridge default exactly as before. A user who
exports any of the three variables keeps their own value (`push_default`).

**Alternatives rejected:**

- **Three rows via any of the six sources above** — impossible without either
  misrepresenting Codex models as Anthropic ones or turning on the discovery
  path #820 forbids.
- **Writing `additionalModelOptionsCache` into `~/.claude.json`.** It would
  render three rows, and it is a cache the harness owns and overwrites; a
  clud that edits it is a clud that breaks on the next refresh.
- **Declaring `ANTHROPIC_CUSTOM_MODEL_OPTION_SUPPORTED_CAPABILITIES`**
  (`effort`, `xhigh_effort`, `max_effort` are real capability tokens) to light
  up the native effort control for the Codex row. Tempting and out of scope
  here: it changes which `output_config.effort` values the harness sends, and
  DD-035 made a stated-but-unsupported effort a 400. It needs its own issue
  and its own upstream check, not a rider on a picker-presentation change.

## DD-039: Bundled skills have exactly one source of truth

**Status:** Accepted

**Context:** clud shipped **two** bundled-skill registries, each with its own
installer, both writing `~/.claude/skills/<name>/SKILL.md`:

| | Registry A | Registry B (retired) |
| --- | --- | --- |
| Constant | `BUNDLED_SKILLS` in `src/skills.rs` | `BUNDLED_SKILLS` in `src/skill_install.rs` |
| Source tree | `crates/clud-bin/assets/skills/` (18) | **mixed** — 7 from `assets/skills/`, 5 from root `skills/` (12) |
| Launch action | `BundledSkillsAction` (ran 1st) | `ClaudeDriftSkillsAction` (ran 2nd) |
| Backends | Claude **and** Codex | Claude only |

Both ran as Global-scope actions in the same startup sequence, so A wrote and
B overwrote. Each installer compared the file on disk against *its own*
embedded copy, classified the other's output as drift, and rewrote it. Every
launch printed `updated /clud-pr` and `updated /clud-issue`.

Neither installer was wrong in isolation. Both were internally consistent and
individually correct — nothing enforced that the two registries owned disjoint
names. That is precisely why the bug survived review and CI: there was no
single artifact anyone could look at and see the conflict.

Three consequences, of which the log noise was the least important:

1. **Stale content silently won.** B ran last, so root copies landed on disk.
   `assets/skills/clud-pr/SKILL.md` (updated 2026-08-03) was overwritten every
   launch by a root copy last touched 2026-06-18. The commit
   `fix(skills): refresh stale bundled skills and retire dead ones (#756)`
   never reached a single user.
2. **Codex and Claude diverged.** A installed to both `~/.claude` and
   `~/.codex`; B only overwrote `~/.claude`. Same skill, two backends,
   different bodies.
3. **`updated` became meaningless.** Firing unconditionally every launch made
   a genuine drift repair — the message's actual purpose — indistinguishable
   from noise.

**Decision:** Bundled skills have exactly one source of truth:
`crates/clud-bin/assets/skills/`, installed by `src/skills.rs`. The root
`skills/` tree and `src/skill_install.rs` are deleted.

`skills.rs` survives because it is strictly more capable: 18 skills vs 12,
multi-backend vs Claude-only, an explicit retirement mechanism
(`PURGED_BUNDLED_SKILLS`) that sweeps every backend, and it was already the
registry CLAUDE.md documented.

Enforcement is `ci/banned_skill_sources.py`, run by `bash lint`:

1. Skill bodies may only be `include_str!`'d from `assets/skills/`.
2. Only `skills.rs` / `skills_home.rs` may build a backend skills path.
3. No second skill source tree at the repo root.

Rule 1 alone would have caught the original bug.

**Alternatives considered:**

- **Keep both, add a disjointness test.** Rejected: it legitimizes two
  installers and only catches *name* collisions, not the divergent bodies that
  caused the visible damage. The duplication is the defect, not a symptom.
- **A dylint.** Rejected on two grounds. Practically, dylint is Linux-only
  nightly and **skipped on PRs**, so drift would merge and be reported hours
  later on `main`. Substantively, these are not Rust-semantic questions —
  "which directory does this path literal point at" and "does a directory
  exist at the repo root" are text and filesystem facts, and the latter no
  Rust lint can answer at all. A compile-free scan is the right tool, matching
  `banned_imports.py` and `banned_cross_tools.py`.
- **Rust unit tests asserting registry/dir agreement.** Dropped as not
  load-bearing once rule 1 exists. Can be added later if wanted.

**Consequences:** `clud-pr-merge` lived only in registry B with no `assets/`
copy, so it had to be migrated *before* B could be deleted — done as a
separate additive PR (#848) so the deletion was provably safe rather than
trusted. Codex gained `clud-pr-merge`, which it never received. Users on
`clud-pr` / `clud-issue` move to the newer `assets/` bodies, which is the
content that was always intended to ship.

The root copies did carry sections the assets copies lack, and those were
checked rather than assumed: `clud-pr`'s *Meta Tracking Issue Mode* is
superseded by `clud-fix`'s Meta/Parent/Burn-Down workflow, and `clud-issue`'s
*What counts as a blocking question* was deliberately replaced by "resolve the
open questions yourself / **Open questions**". Both are superseded, not
missing, so nothing was ported. *(Amended by [DD-040](#dd-040-clud-pr-clud-fix-clud-do-and-clud-pr-merge-are-retired-in-favor-of-goal): the
`clud-issue` decision-discipline content was subsequently judged complementary
rather than superseded, and ported into the assets copy.)*

---

## DD-040: clud-pr, clud-fix, clud-do and clud-pr-merge are retired in favor of /goal

**Status:** Accepted. Builds on [DD-039](#dd-039-bundled-skills-have-exactly-one-source-of-truth); partially reverses #848 (which migrated `clud-pr-merge` into `skills.rs` to preserve it).

**Context:** zackees/clud#844. With DD-039's consolidation landed, the
question remained what to do with the four orchestration skills. Their core
loop — lock a deliverable in, drive to it, refuse to stop early — is what the
harness's `/goal` Stop-hook command does natively. Keeping them meant
maintaining three long playbooks (plus `clud-pr-merge` as a fourth) that
re-implement a built-in, and the two largest (`clud-pr`, `clud-fix`) were the
most cross-referenced skills in the tree.

**Decision:**

- `clud-pr`, `clud-fix`, `clud-do` and `clud-pr-merge` are deleted from
  `assets/skills/` and added to `PURGED_BUNDLED_SKILLS`, which sweeps **every**
  backend's skills dir on next launch (the deleted `PURGED_SKILLS` only ever
  swept `~/.claude`). A user-owned copy (marker stripped) is preserved.
- Surviving skills route orchestration to `/goal`, the worktree/process-audit
  playbook to `clud-git` (which inherits the Windows teardown guardrail test),
  and review delegation to `clud-review`.
- The root-fork `clud-issue` content that DD-039 classed as superseded was
  re-audited and found **complementary**, not superseded — the question
  budget, `## Decisions` issue-body section, blocking-question taxonomy,
  face-value reading, and `--repo` flags are ported into the assets copy.
- `install_to` compares modulo whitespace (`normalize()`, ported from the
  deleted module) so an LF-vs-CRLF difference is not a change, and
  `BundledSkillsAction` announces `[clud] updated /<name>` only for entries in
  the report's `refreshed` list. A current install performs no write at all;
  `real_bundle_install_is_idempotent` and `line_ending_drift_is_not_a_refresh`
  pin both properties.

**Consequences:**

- **A capability is lost, not migrated:** PR Drive Mode (driving an open PR
  through CI failures, review comments and merge conflicts to merge) has no
  `/goal` equivalent. Restoring it means writing a new skill, not reverting
  this change.
- **A whitespace-only edit to a bundled `SKILL.md` no longer propagates** to
  installed homes — the deliberate price of not re-creating #844 on CRLF
  checkouts.
- The four names may be re-introduced later; doing so means removing them from
  `PURGED_BUNDLED_SKILLS` in the same commit that re-adds the bundle entries
  (`retired_skills_are_not_also_bundled` enforces the disjointness).

---

## DD-041: Unified routing is a mode, and model identity has one provider-neutral registry

**Status:** Accepted

**Context:** Issues #898-#901 add a Claude-harness session that can route to
Claude, Codex, and DeepSeek. Before that gateway, clud exposed provider flags,
an independent harness choice, a Codex-only model parser, DeepSeek constants in
the foreground runtime, and a `LaunchMode` type that already meant subprocess
versus PTY. Treating unified as another provider or adding another catalog
would conflate identity and freeze incompatible public state.

**Decision:** `RoutingMode::{Direct, Unified}` is independent from
`ModelProvider::{Claude, Codex, DeepSeek}`, `HarnessSelection`, and the existing
process `LaunchMode`. `provider_catalog.rs` is the single authority mapping
stable clud CLI/settings IDs, gateway discovery IDs, provider wire IDs,
compatibility aliases, and effort/context capabilities. Compound legacy inputs
normalize into separate typed fields before bootstrap. `LaunchPlan` carries the
normalized selection additively with source metadata and no credentials.

The compatibility grammar remains first-class: bare `clud`, permanent provider
flags, provider-before-action composition, and unknown harness passthrough all
remain supported. `run`, `--provider`, `--effort`, and `--context-window` are
owned by clud before `--` and can still be passed literally after it. Unknown
future provider wire IDs remain reachable and are stored byte-for-byte beside
their typed provider identity.

**Consequences:** Direct launches and the unified gateway cannot drift into
different model maps. A provider/model conflict or conflicting legacy/explicit
modifier fails before credentials or paid requests. Repeats pin the normalized
selection instead of re-reading settings. During the dependency-ordered #901
burn-down, unified grammar and wire state may land before the gateway; until
the gateway is enabled, a non-dry unified launch fails locally rather than
silently acting like direct Claude.

---

## DD-042: Unified effort is the harness-resolved session value

**Status:** Accepted

**Context:** Issue #899. Claude Code sends the final effective effort in
`output_config.effort`, after resolving `/effort`, `--effort`, settings,
environment, picker controls, and request-specific overrides. The wire request
does not identify the winning source and `/effort auto` has already been
resolved. Claude, Codex, and DeepSeek expose different defaults and capability
ladders, so a gateway cannot both honor the final value and silently restore a
provider default after `/model` changes.

**Decision:** Unified mode treats effort as one harness-owned session value and
routes each request independently. It does not inject DeepSeek direct mode's
global max overlay and does not remove an ambient
`CLAUDE_CODE_EFFORT_LEVEL`. Native Claude receives the original Messages body.
Codex discovery IDs resolve before the legacy `claude*` fallback and reuse the
existing strict precedence (`@effort`, `output_config`, stated thinking budget,
catalog default). DeepSeek receives the Anthropic effort field unchanged and
owns its documented five-name-to-two-effective-level mapping.

Provider switching also starts a new conversation route epoch. Crossing away
from Codex discards its opaque canonical Responses state; returning to Codex
reseeds from the complete Anthropic-visible transcript instead of appending to
stale provider-private history. Session and subagent identities remain
independent.

**Consequences:** The same effort label can have different latency/cost effects
on different models. Unified mode does not promise to restore Sol `low`,
Terra/Luna `medium`, or a provider's catalog default after a switch because
that source information no longer exists at the gateway. Unsupported Codex
values fail before an upstream call, while DeepSeek-compatible or future values
are not rejected by Codex policy. Direct Claude, Codex, and Codex-via-Claude
launch profiles are unchanged; the direct DeepSeek profile later dropped its
max-effort pin — see [DD-059](#dd-059-direct-provider-launches-carry-effort-on-the-session-flag-and-the-reviewed-default-is-low).

---

## DD-043: Unified launch guards, token counting, and optional-provider notices

**Status:** Accepted

**Context:** Issue #898 left three open edges on the merged unified gateway.
Claude Code discovery silently discards synthetic IDs on clients older than
2.1.223, so a stale install presents the old picker instead of failing. The
gateway had no answer for `POST /v1/messages/count_tokens` in unified mode.
And a launch with missing optional credentials showed fewer picker rows with
no explanation of how to restore them.

**Decision:**

- **Version floor at launch.** Before the child or gateway starts, a non-dry
  unified launch probes the bootstrapped client's `--version`. Outputs older
  than 2.1.223 fail with the installed version and the `claude update` remedy.
  Dry runs skip the probe entirely.
- **Token counting has one Anthropic-compatible contract.** Ordinary Claude
  model IDs proxy to the Anthropic endpoint; synthetic Codex and DeepSeek
  routes return an explicit local 404 so Claude Code falls back to its
  documented local estimation. Unknown reserved IDs still fail locally before
  any upstream request.
- **One sanitized startup notice per missing optional provider.** The
  foreground runtime prints a single actionable line naming the remedy
  (`clud auth login codex` / `clud auth login deepseek`) instead of a silent
  short catalog. Notices never contain secret material, and a missing optional
  credential still never blocks native Claude.
- **Initial selections resolve to discovery IDs.** A launch-time
  `--model`/settings selection for Codex or DeepSeek is emitted to the child
  as the catalog discovery ID, not the provider wire ID: an unrecognized
  `gpt-*`/`deepseek-*` ID would otherwise read as an ordinary Claude ID and be
  proxied to Anthropic.
- **The gateway also resolves persisted wire IDs.** A continued or resumed
  session can still carry a provider wire ID or CLI alias past discovery. The
  gateway resolves every model through the shared catalog before falling
  through to native Claude, so known `gpt-*`/`deepseek-*` IDs route to their
  own provider (and count_tokens answers 404 for them) instead of leaking to
  Anthropic.

**Consequences:** A stale Claude Code install cannot silently show a degraded
picker, and a partial credential setup explains itself at launch. Token
counting either reaches a provider that speaks the contract or falls back to
harness-local estimation; clud never fabricates a count for a synthetic route.
Deprecated `codex-auth`/`deepseek-auth` aliases also print their exact
replacement spelling, including preserved flags, instead of a bare command
name.

## DD-044: Installed harness selection is transient launcher history

**Context:** DeepSeek AI publishes a separate developer-preview harness whose
binary and command grammar (`dsh web`, or `dsh --profile headless <prompt>`)
are distinct from clud's existing DeepSeek-provider-through-Claude route.
Meanwhile, a bare launch had no fast way to choose among multiple installed
agent harnesses without turning a remembered UI choice into routing policy.

**Decision:** Model provider and executable harness identity remain separate.
`--deepseek` retains its existing provider meaning, while
`--harness deepseek` selects `dsh`. Bare interactive launches discover
Claude, Codex, and DeepSeek Harness in stable order. One choice launches
directly; multiple choices use a three-second crossterm countdown selector.
Navigation cancels auto-submit, and confirmation writes
`launcher.last_harness` atomically. That key is not consulted as
`harness.default` and never rewrites provider profiles. Explicit and
noninteractive invocations bypass the selector. DSH installation remains
user-owned while it is a developer preview; clud reports upstream's `npx`
command rather than performing a global npm install.

**Consequences:** Repeated bare launches are quick without silently changing
provider routing, `--deepseek` remains backward compatible, and DSH's unstable
grammar is isolated in the command adapter. The picker must restore terminal
state on every exit, and Claude/Codex-specific options must error for DSH
instead of becoming misleading no-ops.

## DD-045: Direct Codex-through-Claude uses provider-scoped gateway discovery

**Status:** Accepted

**Context:** DD-038's one-row ceiling was correct for Claude Code 2.1.212, but
became stale after Claude Code 2.1.223 admitted clud's reserved
`clud-claude-*` gateway-discovery IDs and #912 implemented that protocol for
unified mode. The direct bridge still emitted `gpt-5.6-terra@medium` through
Claude's `--model` flag. That string was a bridge-private compound selector,
not an OpenAI model ID, so current Claude Code classified it as unknown,
presented one scalar custom row, and enforced its assumed 200K compaction
window despite the GPT-5.6 family's 1.05M context.

**Decision:** The direct bridge exposes `GET /v1/models` with exactly the
three registered Codex discovery rows. Known launch selections are emitted as
their discovery IDs; the bridge resolves those IDs before translation and
sends only the catalog wire ID to OpenAI. Ordinary effort is a separate Claude
session value. The provider-native `none` value remains a discovery-ID suffix
because Claude's CLI does not accept it. The direct child enables discovery,
scrubs the retired scalar custom-row variables, declares the 1,050,000-token
context ceiling, and adopts unified mode's Claude Code 2.1.223 version floor.
An inherited nonessential-traffic kill switch fails the launch because it
would silently disable the required discovery request.

This is provider-scoped discovery, not an alias for `--unified`: direct mode
keeps bearer authentication, Codex credential ownership, plan-mode and Task
suppression, and a catalog containing no Claude or DeepSeek routes. Legacy
wire IDs and `<model>@<effort>` remain accepted for continued sessions and
forward compatibility.

**Consequences:** `/model` honestly presents Sol, Terra, and Luna; the harness
never sees `gpt-5.6-terra@medium`; and auto-compaction no longer falls back to
200K solely because the model name is unknown. Direct and unified gateways now
share the discovery-ID contract while retaining different authentication and
provider-routing boundaries. DD-038's one-row decision remains historical for
pre-2.1.223 Claude Code and is superseded for supported clients.

## DD-046: One catalog row is the model-extension unit

**Status:** Accepted

**Context:** zackees/clud#955. Two design audits followed the direct
Codex-through-Claude failure. The history audit found repeated churn across
model aliases, picker rows, effort transport, defaults, and context handling.
The extension audit traced a hypothetical fourth Codex model through native
Codex, direct Claude, unified Claude, launch plans, repeats, discovery, and
request translation. `provider_catalog.rs` already owned the three identifier
namespaces, but the translator still restated Terra as a default constant, the
direct overlay restated 1,050,000 tokens, and important bridge tests restated
three-element model arrays.

**Decision:** A `CatalogModel` row is the sole production model-extension
unit. It owns the clud/settings ID, provider wire ID, optional Claude discovery
ID, display metadata, aliases, capabilities/defaults, provider-default marker,
and Claude context metadata. Native harnesses consume the wire ID; Claude
gateways advertise the discovery ID and resolve it through the same row. The
Codex translator derives its fallback from the unique reviewed catalog
default. Provider-scoped Claude discovery derives its process-wide context
ceiling only when every advertised row declares the same value.

Conformance tests iterate catalog rows for native and Claude argv, discovery
advertisement, discovery-to-wire routing, and every supported effort. Separate
literal assertions remain only where they intentionally guard billing policy
(Terra is the reviewed default) or a historical upstream fixture. Unknown
reserved discovery IDs fail locally; unknown explicit provider wire IDs keep
the compatibility path.

**Consequences:** Adding a non-default Codex model requires one production
catalog-row edit, followed by normal documentation/review. Missing addressing
or context metadata fails tests or launch locally instead of degrading the
picker, choosing another paid model, or reviving Claude's unknown-model 200K
assumption. A future harness must consume an existing namespace or add one
explicit catalog field; private model tables and display-name inference are
not allowed.

## DD-047: `bash.block_cd` is a first-class setting, not a `bad_commands` rule

**Context:** A `cd` in a Bash tool call mutates the *session* cwd, not just
that command's. Every later tool call inherits the moved cwd, and anything
resolving a relative path against it breaks immediately. Project hooks are the
common casualty: they are conventionally written as repo-relative script paths
(`uv run python ci/hooks/check-on-stop.py`), so drift makes them ENOENT and
the session wedges — no tool can run until a human intervenes. The reported
case drifted *within* the same repo, so "stay inside the repo" is not a
sufficient rule.

Upstream will not fix the underlying behavior: the cwd contract is documented
as following the agent, and the tracker's own reports contradict each other
about whether cwd drifts or silently resets
(anthropics/claude-code#83636, #76708, #84685; the exact failure class in
#50960 and #42282 was closed NOT_PLANNED).

**Decision:** `bash.block_cd: "auto" | true | false` in `.clud/settings.json`,
defaulting to `"auto"`, enforced by `clud-cmd-scan` at PreToolUse.

The existing DD-016 `bad_commands` engine cannot express this. It matches an
executable plus argument-token predicates; deciding whether a `cd` target
escapes the registered roots requires *resolving* the argument against those
roots — path resolution, not pattern matching. A regex rule would either deny
all `cd` (too blunt: `cd src/` is harmless in most repos) or miss `cd ../..`,
`cd $HOME`, `cd %USERPROFILE%`, and absolute paths.

Four properties make the guard safe to default on:

1. **Only session-mutating `cd`s count.** A `cd` inside `(...)`, `$(...)`,
   backticks, or a nested shell runs in a child process and cannot leak, so it
   is always allowed. That keeps the recommended workaround —
   `(cd dir && cmd)` — available under every setting.
2. **`cd` back to a registered root is always allowed**, so a session whose cwd
   has already drifted can recover.
3. **`"auto"` resolves against the environment at fire time**, not at parse
   time: hooks that run relative script paths pin the cwd to the repo root;
   hooks that are all PATH binaries or absolute paths only forbid leaving the
   repo; no hooks means no policy at all.
4. **An unresolvable target is treated per policy, not guessed.** Strict denies
   it (it cannot prove the destination is a root); escape-only allows it (it
   has no evidence of an escape), matching the scanner's general habit of
   narrowing only on evidence.

**Alternatives rejected:** A `bad_commands` rule (cannot resolve paths, per
above). A plain `true|false` toggle — the right answer depends on what the
repo's hooks look like, which clud can determine, so it ships the opinion
rather than a knob. Forcing the cwd back from a `CwdChanged` handler as the
primary mechanism — cwd state is documented-unstable and shared across
concurrent subagents, so nothing may depend on it for correctness.

**Consequences:** Pinning is *hygiene*, not correctness. It protects the
agent's own relative-path commands and keeps the invariant the harness snaps
back to anyway; rooting hooks correctly is the dispatcher's job (#967 Phase
2+), and once a repo's hooks are dispatcher-managed and cwd-immune, `"auto"`
relaxes (#967 Phase 5). Two limits are deliberate and documented rather than
worked around: a `cd` performed by a sourced script is invisible to a
PreToolUse text scan (Phase 5's `CwdChanged` handler is the reactive backstop),
and once the cwd has already escaped, config discovery from the drifted cwd
finds no `.clud/settings.json`, so the policy resolves off — this layer
prevents drift, it cannot force a return.

## DD-048: clud runs a repo's declared hooks itself, and never writes them to a config file

**Context:** Harness hooks execute with the session cwd, which follows the
agent. A hook written as a repo-relative script path — the overwhelmingly
common shape — breaks the moment the agent `cd`s, and a *blocking* hook that
breaks wedges the session outright (DD-047 covers the preventive half; this is
the corrective one). Two adjacent failures share the cause: a nested repo's own
hooks never load, because the harness reads hooks only from the session root,
and the parent's hooks keep firing inside a nested checkout against files they
know nothing about.

**Decision:** A repo declares clud-managed hooks in `.clud/hooks.json`, and
clud executes them, with a fixed rooting contract: cwd and `CLUD_PROJECT_DIR`
are both the declaring repo's root, whatever the session cwd is. The harness's
payload is forwarded on stdin byte-for-byte, with the pipe closed so a hook
blocking in `json.load(sys.stdin)` receives EOF.

Three choices inside that are worth stating:

1. **A separate declaration file, not the harness's own settings.** A hook left
   in `.claude/settings.json` fires natively *and* through clud, and only
   clud's copy is rooted. Declaring is the explicit act that moves a hook from
   the harness's control to clud's; `hook_health` warns when the same command
   appears in both, because migrating means moving, not copying.
2. **Only exit 2 blocks.** A non-2 exit, a spawn failure, or a timeout warns
   and continues. This fails open deliberately: a guard that cannot run is a
   bug in the guard, and converting it into a wall in front of every tool call
   reproduces the exact wedge the feature exists to prevent. A blocking hook's
   stdout is relayed verbatim rather than re-wrapped, since it may be speaking
   the harness's own JSON protocol.
3. **A bare `clud-cmd-scan` still means `PreToolUse`.** Every already-installed
   hook line is bare; changing what those mean would silently repoint every
   existing user's guard. Other events are named with `--event <Event>`.

**Consequences:** Hooks become cwd-immune for repos that migrate, which is what
later phases build on — the typed root registry needs a loader it controls
before it can root a sub-repo's hooks at the sub-repo. Repos that do not
migrate are unaffected; discovery costs one `is_file` probe. clud is now
responsible for a contract the harness used to own, so divergence in matcher
semantics or exit-code handling is a real risk, mitigated by mirroring the
harness's rules and asserting them directly in tests.

## DD-049: settings reach the harness as compiled CLI arguments, never as file writes

**Context:** Delivering clud's hook set to the harness needs the harness to
read it from somewhere. The obvious route is writing hook lines into a settings
file — the checked-in `.claude/settings.json` (a dirty working tree on every
launch), or the gitignored `.claude/settings.local.json`.

**Decision:** clud compiles its settings into each frontend's native
configuration surface and passes them **as command-line arguments at launch**.
Claude takes `--settings <file-or-json>`, an *additional* source that merges
with the settings files rather than replacing them, with hook entries
concatenating across levels; codex takes repeated `-c key=value` overrides.
Neither path writes to a file the user owns.

clud already did exactly this for the bridge routes — `foreground_runtime.rs`
composes a settings document, writes it to a session-lifetime tempfile, injects
`--settings`, and merges into a user-supplied `--settings` so neither shadows
the other — so this generalizes an existing mechanism rather than inventing
one.

**Alternatives rejected:** Writing managed lines into a settings file, in any
location. It carries an idempotence requirement, a read-modify-write
lost-update risk (every existing writer rewrites the whole file, and only one
of them takes `~/.clud/settings.lock`), the two-writers-one-file fight that
caused #847, a per-repo assumption that the target is gitignored, and stale
state left behind when a session is killed. Argument injection has none of
those: there is nothing on disk to converge, and the tempfile dies with the
session.

**Consequences:** The Claude path becomes file-free.

*Update, verified while implementing (#967 Phase 2b):* the codex half of this
is worse than "no `--settings` equivalent" — codex has **no argument surface
for hooks at all**. `-c key=value` overrides values that would otherwise load
from `config.toml`, and codex hooks live in a separate `hooks.json` with no
flag pointing at an alternate one; `CODEX_HOME` would relocate auth and config
along with it. So the choice for codex was to write `~/.codex/hooks.json` or to
accept what the already-installed `clud-cmd-scan` PreToolUse line gives. clud
takes the second: codex keeps PreToolUse coverage (which runs declared hooks
since #980) and gets nothing for other events, matching codex's own apparent
single-event support. No second codex writer exists. One route deliberately left open: Claude's
`--setting-sources` can *exclude* a settings source outright (verified on the
shipped CLI — a project `SessionStart` hook fires under
`--setting-sources user,project` and does not under `--setting-sources user`),
which would let clud absorb a repo's existing hooks and fix repos with no
migration at all. Not taken yet, because excluding a source drops everything it
contributes: a bug there costs the user `permissions`, a security control, not
just hooks. It is strictly additive to this decision, so deferring it is free.
## DD-050: Route health is a second question, and failover is opt-in and cost-labeled

**Context:** #968. A session on OpenRouter's free daily tier ran out mid-loop.
Every later request died `429 ... free-models-per-day-stealth`, four scheduled
`/loop` wakeups burned against a dead account, and three `/model` switches
failed identically — because the model picker moves the model ID, not the
upstream. The base URL is fixed at process launch, so nothing reachable from
inside the session could leave the drained account. The only exit was killing
the session and paying a full uncached context re-read on `--resume`.

**Decision:** three choices that are not obvious from the code alone.

**1. Route health is a separate question from retry, and needs its own module.**
`codex_upstream` already answers "can this *attempt* succeed if I try again
right now?" and then discards the answer. Routing needs a longer-lived one:
"can this *route* serve at all, and until when?" The two are not the same
question. A drained account and a malformed request are both
`FailureClass::Permanent` to the retry loop and could not be more opposite to
a router — the first must move traffic elsewhere, the second must **never**
move it, because the next route would reject the same bytes and charge a
second account to do it. `route_health::RouteVerdict` names the six distinct
routing decisions; collapsing any two either strands a session on a dead route
or replays a bad request onto a paid fallback.

The ledger is launch-scoped, not global: a wedged account in one session must
never suppress a route in another. Clocks are passed in rather than read, so
every rule is testable without sleeping.

**2. The ladder is configured, never guessed, and every rung declares who
pays.** The default holds exactly one rung — the selected route — so a user who
has not asked for failover sees no change in behavior and no change in spend.
Descending onto a `CostOwner::Metered` rung requires consent recorded once
(`--failover-allow-metered`). Automatic recovery must not become an automatic
invoice; that is a worse failure than the outage being fixed.

Ordering matters and is the caller's: a switch inside one provider family costs
only a cold prompt cache, while crossing families additionally costs a Codex
reseed. Same-family rungs belong first.

**3. Replay happens only before commit.** `serve_messages` already documented
the seam — *"The status is chosen only while nothing has been written."* Before
the first frame the status is still ours, so a route-terminal failure is
re-issued against the next rung and the client sees one ordinary `200`. Context
survives trivially because Claude Code sends the full Anthropic-visible
transcript on every request: the gateway forwards what the client sent rather
than reconstructing it. After commit the status is spent, so the honest answer
is to end the turn — one turn lost, never the conversation.

**Rejected: a standalone `clud route status` command.** The gateway is
launch-scoped and its port and token are never serialized — the property that
keeps a launch's credentials off disk — so a separate process has nothing to
connect to. Building the CLI would require publishing a discovery file the
design deliberately avoids. Health is exposed on the gateway itself at
`GET /_clud/route/status`, beside the existing `/_clud/context/*`, with
`POST /_clud/route/clear` as the escape hatch for a clock-less drain (a spent
balance has no reset time, so after a top-up nothing else brings it back).

**Rejected: shrinking `max_tokens` to fit a balance.** The observed `402` reads
"requested up to 32000 tokens, but can only afford 1600". Trimming the request
to fit is the obvious-looking fix and the wrong one: it converts a billing
failure into a silently truncated answer.

See [`architecture/provider-failover.md`](architecture/provider-failover.md).
---

## DD-051: Daemon creation is granted by a positive launch capability

**Status:** Accepted

**Context:** The old `CLUD_NO_DAEMON=1` convention made daemon creation the
default for every newly added command path. A tool or hook invocation only
remained safe if every dispatcher and every child environment remembered to
set the negative flag. Version skew made that default dangerous: an older clud
reached through `ensure_daemon`, classified a newer daemon as merely
"different", and could terminate it while trying to replace it.

**Decision:** Daemon creation and replacement require the process-local
capability `CLUD_ALLOW_DAEMON_SPAWN=1`. Every clud process clears inherited
copies immediately after argument normalization. Only the normal command-less
backend launch path (including the normalized `clud run` spelling) sets it,
after utility, tool, auth, maintenance, daemon-control, and internal modes have
already dispatched. Tool children also strip the capability from their
materialized environment. Existing compatible daemons remain usable without
the capability; only daemon-state mutation is gated. `--no-daemon` remains the
explicit CLI opt-out. This supersedes the `CLUD_NO_DAEMON` portions of DD-011
and DD-012.

Daemon shutdown is independently generation- and version-guarded. A shutdown
request carries the caller version and the expected daemon PID plus start time;
the daemon rejects legacy/unversioned callers, older callers, and requests for
a different generation.

**Consequences:** Adding a new subcommand cannot accidentally gain daemon-spawn
authority. `clud tool` and hook chains cannot inherit it. A utility mode may
talk to an already-running compatible daemon but gets a local permission error
if its operation would need to create or replace one. An older client that
encounters a newer daemon leaves it untouched, emits the yellow compatibility
error, and exits 1.

## DD-052: hook applicability is decided by a root's relationship to the session, not by path geometry

**Context:** A session touches files in more than one repo — the repo it was
launched in, temporary checkouts clud clones under `.extern-repos/`, and
organizational sub-repos a project declares. "Which hooks apply here" needs an
answer that does not reduce to "which directory is this under", because every
one of those lives *inside* the parent's tree.

**Decision:** Roots are registered with a **kind**, and the kind decides the
firing rule:

| kind | registered by | parent hooks fire there |
| --- | --- | --- |
| `parent` | the session root | yes |
| `extern` | immediate children of `.extern-repos/` (implicit) | **never** |
| `child` | declaration in `.clud/settings.json` | yes |
| unregistered | — | no |

An `extern` root is a temporary, foreign visit: the parent's guards are
meaningless against a repo it does not own and will not keep, and firing them
there is precisely the #841 ENOENT wedge. A declared `child` is the opposite —
part of the parent's world, so the parent's guards apply to it and its own
hooks run rooted at it.

Nested git repos are **not** auto-detected as children. Declaration is the
consent that makes the child tier's no-prompt trust sound (D8), and that
reasoning collapses when nothing was declared; a vendored dependency or a
stray clone would otherwise become a trusted root by accident.

**Containment comes from what a call names, never from cwd alone.** A subagent
editing `.extern-repos/<sub>/src/lib.rs` typically still has the session cwd at
the parent root, so a cwd-keyed rule would answer "parent" for a file that is
plainly not the parent's. Resolution order: the paths a tool names
(`file_path`, `notebook_path`, `path`); otherwise, for Bash, wherever the
command would `cd` to, because `cd .extern-repos/dep && make` does its work in
the sub-repo and cwd is only where it started; otherwise cwd. A call that spans
repos still earns the parent's guards for the parent's own files — any touched
path the parent owns is enough.

**Alternatives rejected:** A symmetric path-scoped rule ("a repo's hooks fire
for its own files, full stop"), which was the earlier draft. It cannot express
the difference between a visitor and a child, and those need opposite
parent-hook behavior. Auto-detecting nested git repos as children, which is
convenient and unsound for the reason above.

Also rejected, and worth naming because it is the shape one reaches for first:
treating `cd` targets as *additional* touched paths alongside cwd, then asking
whether **any** touched path is parent-owned. That keeps answering "yes" for
`cd .extern-repos/dep && make`, because cwd is still the parent — so the
parent's guards fire inside the sub-repo, which is exactly the failure this
tier exists to prevent. The targets have to **replace** cwd, not join it. This
was written, caught by an end-to-end test, and is now locked by
`a_cd_target_replaces_cwd_rather_than_joining_it`.

**Consequences:** The registry has to reach the hook process, and two of its
inputs — `--add-dir` targets and `permissions.additionalDirectories` — appear
in no hook payload, so clud carries them in `CLUD_HOOK_ROOTS` as JSON (a
path-separated list is ambiguous on Windows, where paths contain `:`). Roots
are matched most-specific-first, so a sub-repo nested inside the parent wins
its own containment lookup regardless of registration order. `bash.block_cd`
pinning now targets the whole registered set, so moving between the parent and
a registered sub-repo is allowed while wandering into an unregistered
directory is not.

## DD-053: foreign checkouts live beside the repo, not inside it

**Context:** clud cloned dependent repositories into `<repo>/.extern-repos/`.
Anything under the repo root has to be excluded by every tool pointed at that
root — linters, formatters, test collectors, IDE indexers, file watchers, build
scripts — and the list is unbounded, per-repo, and manual.

Measured on one developer machine: 23 repos carried the directory, 16 of them
empty husks left after GC removed their contents. The largest held **27,712
files** inside a repo. It had also stopped being a clone location and become a
dumping ground — scraped `.html` files in one repo, a `codex.tar.gz` in
another — and every worktree got its own copy of the same dependency.

The decisive evidence was in this repo's own lint script. `ci/banned_imports.py`
listed `extern-repos` among the directories it skips, but the directory is
`.extern-repos`; `Path.parts` yields the component verbatim, so the membership
test never matched. **The exclusion had never fired.** Every `bash lint` run
walked into every cloned dependency and scanned its Python. Nothing went red —
the symptom was only a slow lint and the occasional finding in somebody else's
code — which is exactly why it survived. Fixed separately in #987.

A probe across three layouts made the boundary precise. With the `.gitignore`
entry present, `git`, `ripgrep`, `ruff` and `pytest` all skip an in-tree
checkout — because `.extern-repos` is dot-prefixed *and* gitignored, two
coincidences rather than a design. Remove the entry and `ruff` walks in. And a
plain `Path('.').rglob('*.py')` walks in **either way**: it respects no ignore
file, and it is what repo CI scripts and build systems do.

**Decision:** Checkouts live in a sibling directory derived from the repo's own
name — `~/dev/myrepo` keeps them in `~/dev/myrepo-extern/`. No tool pointed at
the repo can reach them, so there is no exclusion to maintain and none to get
silently wrong.

Three details:

1. **Derived from the main repo root**, so `~/dev/myrepo` and a worktree at
   `~/dev/myrepo-wt-123` share `~/dev/myrepo-extern` instead of cloning the
   same dependency once per worktree.
2. **Claimed with a marker.** The name is guessed from the repo's own, so it
   might already be somebody's real project. clud writes a marker naming the
   owner and refuses to adopt a non-empty directory without one.
3. **The legacy location stays readable.** Discovery and the clone guard both
   still accept `<repo>/.extern-repos/`, so existing checkouts keep working
   while users move them.

**Consequences:** Containment becomes a **disjoint** question instead of a
nested one. DD-052's firing rule needed most-specific-first root matching and a
"parent hooks never fire in an extern root" rule stated as an exception,
precisely because the checkout sat inside the parent's tree. A sibling is
outside it, so the two sets never overlap and the rule follows from the layout.

Two costs are real. The location is now **fallible** — a repo at a filesystem
root has no parent to hold a sibling — where `repo_root.join(...)` never was;
callers get `Option` and the clone guard falls back to the legacy path rather
than becoming permissive. And a sibling is outside the project directory, so
the agent needs `--add-dir` to read what it just cloned; clud already harvests
and injects add-dirs (#967 Phase 3b), so that composes rather than adding a
mechanism.

GC needed a fourth watch root rather than a widening of the existing
sibling-clone one: that scanner inserts immediate children of the repo's
*parent*, while `<repo>-extern/dep` is a grandchild of it. Registry rows were
already keyed on absolute paths, so tracking and sweeping were unaffected.

**Alternative rejected:** a central cache at `~/.clud/extern/<repo-key>/`. It
solves the tooling problem equally well and avoids name collisions entirely,
but the path stops being self-describing — a user looking for what the agent
cloned has to know the hashing scheme instead of looking next to their repo.
For a directory users are expected to inspect and delete by hand, adjacency is
worth more than collision-freedom.

## DD-054: The model picker belongs to the harness, and discovery only adds rows

**Status:** Accepted

**Context:** zackees/clud#995 reported a bridge-routed session wedged by an
ordinary `/model` selection: every turn failed with *There's an issue with the
selected model (`claude-opus-5[1m]`)* until the model was changed back.
zackees/clud#997 asked which of three things was true — discovery is not
consulted by the picker, it is consulted and merged with Claude Code's built-in
list, or the advertised set never reaches the picker.

The bridge's served set was already correct and constrained. `serve_codex_catalog`
and `serve_unified_catalog` (`codex_bridge.rs:1091`, `:1062`) answer
`GET /v1/models` from `provider_catalog::MODELS` filtered to rows carrying a
`discovery_id`, and `serve_unified_catalog` maps `ModelProvider::Claude => false`
outright. `claude-opus-5[1m]` is not a catalog row at all. So the ID did not come
from clud.

**The mechanism, read out of the Claude Code 2.1.233 binary.** The picker's
option list is assembled by one function that seeds a list from Claude Code's
built-in Anthropic lineup and then appends to it, once per source:
`ANTHROPIC_CUSTOM_MODEL_OPTION`, the gateway-discovered rows, the
`additionalModelOptionsCache` entries from Anthropic's bootstrap response, the
`availableModels` settings allowlist, and finally the currently selected model.
Every source pushes. **None filters the seed list.** The discovery helper returns
its rows for appending and drops any whose equivalent is already present.

Anthropic's [gateway protocol
reference](https://code.claude.com/docs/en/llm-gateway-protocol#model-discovery)
states the same contract: discovery "add[s] the returned models to the `/model`
picker", and if it fails "the picker falls back to the cached list from the
previous startup or to the built-in model list".

Discovery is additionally gated on the deployment mode being `firstParty`, which
is what a bare custom `ANTHROPIC_BASE_URL` yields — no `CLAUDE_CODE_USE_*`
provider variable is set. That is the same condition under which the built-in
lineup is emitted. **The precondition for discovery running at all is the
precondition for the built-in rows existing**, so they cannot be separated from
the gateway side.

The observed ID follows from the same reading. The built-in extended-context row
carries the alias value `opus[1m]`; a separate pass rewrites alias rows to
explicit first-party IDs whenever the user's `modelAccessCache` is non-empty or
an `availableModels` setting exists, turning `opus[1m]` into `claude-opus-5[1m]`.
Both inputs are the harness's, persisted in the user's global config from
ordinary Anthropic-authenticated sessions and refreshed behind clud's back.

Hypothesis 3 is ruled out empirically, not merely structurally: after a bridge
session, `~/.claude/cache/gateway-models.json` on the reporting machine held
exactly `clud-claude-codex-sol`, `-terra`, and `-luna` — the three rows
`serve_codex_catalog` serves, validated and cached under the bridge's loopback
base URL. The advertised set arrived intact and was still merged with the
built-ins rather than replacing them.

**Decision:** Record that the `/model` picker cannot be constrained from clud's
side, and stop documenting the opposite. Discovery is enabled and correct; its
contract is to *add* honest rows, not to bound the list. clud does not own the
picker and has no supported lever that subtracts from it.

The workarounds were each checked and each fails:

- **Declare a non-`firstParty` provider mode** to shed the built-in lineup. This
  disables discovery outright, so clud's own rows disappear with them.
- **`availableModels`.** Upstream documents it as bounding what *discovery* may
  add; it does not bound the built-in lineup, and on its own it only ever adds
  `claude-*` and `anthropic.*` IDs. Setting it also forces the alias-to-explicit
  rewrite described above, which makes the reported ID *more* likely to appear,
  not less.
- **Writing `additionalModelOptionsCache` or `modelAccessCache`.** Harness-owned
  caches that the harness overwrites on its next bootstrap. Already rejected for
  the same reason in [DD-038](#dd-038-the-codex-picker-gets-one-honest-row-always-carrying-the-catalog).
- **Tier hijacking** via `ANTHROPIC_DEFAULT_*_MODEL`. Rejected in DD-035 and
  DD-038 because it burns the Anthropic names and lies about what is running.

**Consequences:** #995's remedy is entirely detection and reporting —
zackees/clud#998 (a `failure_reason` on `LaunchRecord`), #999 (log the discovery
handshake), and #1000 (distinguish "not in the catalog" from "in the catalog but
not advertised"). No picker-constraining work is available to schedule, and this
record exists so that conclusion is not re-derived.

**Correction (same day, after zackees/clud#1005).** The first draft of this
record claimed a built-in Anthropic pick "reaches a gateway that cannot serve
it" on the direct route. That is wrong. `resolve_selection`
(`codex_translate.rs:748`) maps any requested ID starting with `claude` onto the
route's configured default, so such a pick is *served* on a Codex model — the
silent substitution [DD-038](#dd-038-the-codex-picker-gets-one-honest-row-always-carrying-the-catalog)
already recorded. Unresolvable non-`claude*` IDs are refused with a 400 before
the translator. **The mechanism of the `claude-opus-5[1m]` wedge reported in
#995 is therefore still unestablished**, because nothing logged it; that is what
#998 and #999 exist to fix. The decision above is unaffected — it concerns
whether the picker can be constrained, which it cannot.

This narrows, but does not overturn, [DD-045](#dd-045-direct-codex-through-claude-uses-provider-scoped-gateway-discovery).
Its decision stands: the direct bridge exposes exactly the registered Codex
discovery rows, and `/model` presents Sol, Terra, and Luna honestly. What DD-045
left implicit is that the picker presents them *in addition to* Claude Code's own
rows, not instead of them.

## DD-055: API logical sessions are durable records above worker generations

**Status:** Accepted

**Context:** A daemon `SessionSnapshot` represents one worker process. Its PID,
attach socket, and exit code are intentionally worker-lifetime fields, and
existing reconciliation retires crash-leftover worker records. The API session
surface needs a provider conversation to survive normal turn completion and
daemon restart.

**Decision:** Persist `ApiSessionRecord`s separately under `api-sessions/`.
They own immutable canonical CWD, resolved settings, provider identity, logical
state, turn generations, bounded cursor events, and bounded idempotency. A
worker/process identity, when later recorded on a turn, is diagnostic only
after restart: the restarted daemon marks an active turn failed and never
signals a PID recovered from disk.

**Consequences:** Normal completion becomes `idle`, not a terminal worker exit,
and later lifecycle work can resume only a captured provider identity. The
existing attach/list/kill worker machinery remains compatible because it does
not accidentally classify logical API sessions as attachable workers. Bounded
event and idempotency retention prevents durable session metadata from becoming
an unbounded prompt/transcript store.

## DD-056: the command gate is an allowlist, and it fails closed

**Status:** Accepted

**Context:** #963's abstract interpreter (`block_bad_cmd_rm_vars.rs`) defends
against catastrophic deletes by *proving*, from command text, that a path
variable holds one nonempty literal path. That is 1100 lines answering a hard
question, and every parser bug in it is a bypass. The wider `bad_commands` /
`bad_pipelines` policy is a denylist: it enumerates bad shapes, so a gap in the
enumeration is a hole. Both fail open by design, which is correct for a
"friction-reducing nudge" but leaves no shape that is actually load-bearing
after a real incident.

**Decision:** Add a gate that requires every statement in a shell tool call to
be invoked through a wrapper (`tap` by default). The wrapper runs *after* shell
expansion, so it observes the real argv — an unset variable has already become
`/` and there is nothing left to prove. The hook's remaining job is the much
smaller one of guaranteeing the wrapper is on the path of every command.

Three properties follow deliberately, each inverting a surrounding convention:

1. **Allowlist, not denylist.** One command shape is permitted; everything else
   is denied. There is no enumeration to leave a gap in.
2. **Fails closed.** When `CLUD_CMD_GATE` is set to `enforce`, `1`, or `on`
   (surrounding whitespace ignored; anything else, including unset, leaves
   the gate off), `run_for_event`'s
   allow-by-default exits (unreadable stdin, empty/undecodable/unrecognized
   payload) become denials. A payload the hook cannot read is a command it
   cannot verify.
3. **Refuses what it cannot decompose.** Command substitution, subshells,
   process substitution, and control flow are denied rather than analyzed,
   because they run programs the gate would never inspect.

Property 3 is affordable only because of an asymmetry: when the interpreter
cannot decide, denying blocks legitimate work, so it is tuned to allow; when the
gate cannot decide, denying costs one extra tool call. Uncertainty is cheap
here, so the scanner spends it freely.

The gate does not reuse `command_words`, despite the overlap. That helper
unwraps `env`, `exec`, `command`, and `sudo` so denylist rules can find the real
program underneath — precisely the behavior that would let `env tap ...` satisfy
a prefix check. Reusing denylist machinery inside an allowlist reintroduces the
enumeration problem the allowlist exists to escape.

**Consequences:** Compound commands must be split across tool calls, and
control flow, command substitution and heredoc-free pipelines that mix wrapped
and unwrapped stages are refused; agents adapt to this from the denial message.
Coverage is depth-1: `tap make` does not confine the Makefile, which matches the
threat model (an agent slip in the tool-call string) but is not containment —
that remains a sandbox's job. Redirections are performed by the shell, not the
wrapper, so `tap cmd > "$VAR/out"` can still write to a mis-expanded path; a
redirect touches one file and cannot recurse, and `set -u` in the session shell
is the proportionate mitigation (shipped in #1066; see DD-067). Coverage is also per-session: only sessions
where clud set `CLUD_CMD_GATE` are gated, which is why `block_bad_cmd_rm_vars`
stays in place rather than being retired on arrival.

### Rollout, and how to turn it off (#1067)

`tap` v0 ships as the `tap` binary in `crates/tap`. It is not on by default.
clud sets `CLUD_CMD_GATE` only when you opt in with `CLUD_CMD_GATE_AUTO` (step 3
below).

The gate is controlled entirely by two environment variables, both read by
`block_bad_cmd_gate`:

| Variable | Effect |
|---|---|
| `CLUD_CMD_GATE` | `enforce`, `1`, or `on` turns the gate on. Anything else, including unset, leaves it off. |
| `CLUD_CMD_GATE_PREFIX` | The required wrapper. Defaults to `tap`. |
| `CLUD_CMD_GATE_AUTO` | Read by clud, not the gate: `1`/`true`/`yes`/`on` makes clud set `CLUD_CMD_GATE=enforce` in the session it launches, **only if** the wrapper resolves on that session's `PATH` and `CLUD_CMD_GATE` is not already set. |

**Disabling returns to post-#1064 behaviour exactly.** Unsetting
`CLUD_CMD_GATE` (and `CLUD_CMD_GATE_AUTO`, if you opted in) is the whole revert: the gate's own entry point short-circuits
on it before inspecting anything, so no other code path changes. Removing the
`tap` binary is not required and does nothing on its own -- an enabled gate
with no `tap` on `PATH` refuses everything, which is the failure-closed
direction but not a useful state.

The enablement sequence in #1067 is deliberately staged:

1. **Ship `tap` and dogfood it opt-in.** Done: `CLUD_CMD_GATE=enforce`.
2. **Measure the false-positive rate.** Done, by replay rather than a dogfood
   log: `clud-bench-gate-replay` (`--features bench`) runs every Bash command
   recorded in Claude Code transcripts through the gate's own classifier
   (`block_bad_cmd_gate::classify`) and reports counts only. On 2,538
   transcripts / 121,745 commands (2026-09-25):

   | Class | Share |
   |---|---|
   | Passes as written | 0.6% |
   | Passes once its one statement is prefixed | 11.9% |
   | Passes once each of several statements is prefixed | 71.8% |
   | **Refused even fully wrapped** (needs restructuring) | **15.7%** |

   The refused 15.7% is mostly command substitution (12,589), control flow
   (4,238) and subshells (1,801). A replay measures commands agents wrote
   *without* the gate, so it is the friction an agent would meet on first
   contact, not after it adapts. It is also the floor for step 4's decision.
3. **Opt-in, from clud.** Done: `CLUD_CMD_GATE_AUTO=1`. It is a child-env
   layer (`shell::cmd_gate`, next to nounset) rather than a `LaunchPlan`
   field, because `apply_child_env_policy` is the one builder both the
   foreground runner and the daemon use; a detached session is gated exactly
   like a foreground one with no new daemon wire field.
4. **Default-on — not taken.** One command in six would be refused outright
   even from an agent that wraps everything, and 72% would need several
   prefixes. That is not an affordable default. Revisit only with a gate that
   admits more shapes (for example, wrapped pipelines or `$(...)` whose body is
   itself wrapped), re-measured with the same replay.

Step 2 is not ceremony. The gate refuses compound commands, control flow, and
command substitution outright; that is affordable only if the rate at which it
refuses legitimate work is measured rather than assumed. Skipping to step 4
would make that assumption on every user's behalf at once.

## DD-057: a hook that cannot verify its payload denies removals and allows everything else

**Status:** Accepted

**Context:** `block_bad_cmd` is "a friction-reducing nudge, not a security
sandbox". It fails open on purpose, and `hook-dispatch.md` explains why: a guard
that cannot run must not wall off every tool call, because a wedged session is
the outcome the whole subsystem exists to prevent. `run_for_event` accordingly
had three unconditional allow exits — stdin truncated, payload undecodable,
payload shape unrecognized.

That default is wrong for exactly one class of command. #963 built an
interpreter that proves a removal's path variable holds one nonempty literal
path, but the interpreter only runs if the payload parses first. In the incident
behind #1064, it did not: a hook that could not read its input silently allowed
an `rm -rf "$VAR"/` that expanded to `rm -rf /`. Every other allowed-in-error
command can be undone. A recursive delete cannot.

**Decision:** Invert the default for removals only, on `PreToolUse` alone. When
the payload cannot be decoded or its shape recognized *and* the raw stdin bytes
name a removal program, deny. Everything else still fails open.

A read that stopped before EOF is deliberately *not* a trigger. It is recorded,
and it names the reason in the denial message, but on its own it means nothing
is wrong: Claude Code routinely writes a complete payload and then leaves the
pipe open (anthropics/claude-code#53177), which is why the idle timeout exists
at all. A genuinely truncated payload cuts a JSON string mid-flight and cannot
decode, so the decode check already covers it. An early draft treated the open
pipe as unverifiable and thereby denied every tool call whose text merely
mentioned `rm` — including #963's own safe-rewrite path — with retry advice that
could never succeed. That is the anti-wedge property failing, which is worse
than the bug being fixed.

Two consequences follow, and both are deliberate.

*The probe reads raw bytes, and is not the interpreter's probe.*
`contains_removal_program_text` reads shell command text that has already been
extracted from a payload. Here there is no extracted command — parsing is what
failed — so the input is a possibly-truncated JSON fragment, and three of its
properties break that probe: a newline inside a JSON string is the two
characters `\` and `n` rather than whitespace, so `cd /tmp\nrm -rf $SP/` reads a
literal `n` before the `rm`; truncation can stop the bytes immediately after
`rm`, leaving no following character; and the program may be named by path
(`/bin/rm`). `raw_payload_mentions_removal` therefore matches the removal as a
*word* — escapes collapse to separators, end-of-input is a boundary, a leading
directory is stripped. The interpreter's probe is left alone so #963's decisions
do not move.

*The probe over-matches on purpose.* `git rm` in a commit message trips it. That
costs one retry; under-matching costs a filesystem, and the probe only ever runs
on payloads that already failed to parse, so the false-positive population is
tiny to begin with.

**Consequences:** A removal whose payload the hook cannot read is refused, and
the agent retries it — in the worst case as its own tool call with literal
paths. The anti-wedge property is preserved for every other command and is
pinned from both sides: `unverifiable_payload_without_a_removal_still_fails_open`
covers a broken payload that has nothing to do with removal, and
`test_complete_payload_is_verified_even_when_stdin_never_reaches_eof` covers the
held-open pipe that actually regressed. A
regression there is worse than the bug this fixes, because it would wall off
every tool call whenever the hook hiccups. Truncation is genuinely exceptional
(a 1 MB cap, a 0.25 s idle timeout, a 2 s deadline), so this path is not on the
common route.

This is narrower than the command gate's inversion ([DD-056](#dd-056-the-command-gate-is-an-allowlist-and-it-fails-closed)),
which denies *everything* it cannot verify but only under `CLUD_CMD_GATE`. This
one needs no configuration and is always on, which is why it is scoped to the
single class where allow-on-error is unrecoverable.

## DD-058: the rm-variable interpreter reasons over resolved values, not literal token shape

**Status:** Accepted

**Context:** #963's interpreter and its #1064/#1068 hardening keyed the hazard on
the **literal token shape `$VAR/`** present in the command text: it value-checked
a removal operand only when a `/` sat textually adjacent to a recognized
expansion, and it identified the removal program by a literal `rm`/`rmdir`
token. A red-team sweep (#1070) showed that assumption is bypassable in many
unrelated ways, each confirmed to expand to `rm -rf /` in real bash while the
guard allowed it: the program named through a variable (`R=rm; $R -rf "$V"/`),
the slash or the whole root carried inside a value (`D=/; rm -rf "$D"`), the `/`
synthesized by an unmodeled parameter operator (`${V:0:1}`), ANSI-C quoting
(`$'\x2f'`), `$IFS` word-splitting, tilde and brace expansion, and a provable
rewrite that disabled the cross-statement fallback for its siblings.

**Decision:** Move the interpreter from *token-shape* reasoning to *value-flow*
reasoning (#1071–#1078, #1088):

- Resolve the program word through the same value/substitution model as
  operands, so a variable- or substitution-built `rm` is recognized.
- Value-check every removal operand after substituting known values, and
  propagate a `Hazard` value for a variable assigned an unprovable base — so a
  root reaches the check regardless of where the `/` came from.
- Decode ANSI-C `$'...'` in the lexer; treat unmodeled `${...}` operators,
  unquoted `$IFS`, leading tilde, and brace groups that can expand to a root as
  hazards.
- Run the `unproven_hazard_reason` fallback unconditionally, so a provable
  rewrite in one statement no longer suppresses the sweep for another.

**Scope broadened, but deliberately still incomplete.** The recursive-delete
verb set is widened past `rm`/`rmdir`/`find -delete` to a **curated, best-effort**
denylist of known idioms — Perl `File::Path` `rmtree`/`remove_tree`, Python
`shutil.rmtree`/`os.removedirs`, `rsync … --delete`, `find … -exec <deleter>`
(#1079) — fired only when a `$VAR/`-rooted operand is present. This is explicitly
not exhaustive: any interpreter can delete a tree, and enumerating them is a
losing race. Likewise the guard still reasons about command *text* before the
shell expands it, so constructs that only reveal their argv at runtime remain
out of reach. Those residual classes are why the post-expansion wrapper (#1067,
`tap`) and `set -u` (#1066) remain the durable answers; this decision closes the
text-time gaps that are closable and records that the rest are not.

**Consequences:** The benign corpus is unchanged — proven-literal removals still
rewrite, and near-misses (`echo $'\x41'`, `rm -rf ./~backup`, `rm -rf {a,b}.txt`,
`git rm`) stay allowed — pinned by `stress_benign_commands_are_not_swept_up`.
Each closed bypass is a regression test in `block_bad_cmd_rm_vars.rs`.

---

## DD-059: Direct provider launches carry effort on the session flag, and the reviewed default is low

**Status:** Accepted

**Context:** A `clud --deepseek` session reported that `/effort` was overridden:
the direct Anthropic-compat overlay (`apply_anthropic_compat_overlay`) pinned
`CLAUDE_CODE_EFFORT_LEVEL` to the selection's effort — `max`, once the catalog's
reviewed DeepSeek default applied. Claude Code treats that env var as a locked
override that beats `/effort`, `--effort`, and settings, so the picker could
never move the session. The pin was also redundant: `command::builder` already
emits `--effort <level>` whenever the resolved selection carries an effort,
catalog defaults included. The unified overlay (DD-042) already had the right
rule — preserve an ambient value, inject nothing — making direct mode the odd
one out.

**Decision:**

- The direct overlay neither injects nor scrubs `CLAUDE_CODE_EFFORT_LEVEL`.
  The catalog default effort travels on the harness's own `--effort` session
  flag, which sets the *initial* value and leaves `/effort` live for the rest
  of the session. An ambient user-exported value is preserved and, per the
  harness's own precedence, wins over the flag — the user's own pin is their
  choice.
- The reviewed default effort for every Anthropic-compat direct row is now
  `low` instead of `max`: DeepSeek Pro and Flash, Kimi K3, and the OpenRouter
  Sonnet row all set `default_effort: Some(EffortLevel::Low)`. Flash
  previously had no default at all, which silently fell through to the
  overlay's `max` fallback; the catalog is now the single source of truth for
  direct-mode defaults.
- Unified mode is unchanged (DD-042): the harness resolves effort, clud
  applies no catalog default, and DeepSeek's documented five-name-to-two-level
  wire mapping still owns the server side.

**Consequences:** `/effort` is the live per-turn control in direct DeepSeek,
Kimi, and OpenRouter sessions, exactly as in unified mode. Sessions that
relied on the old implicit `max` now start at `low` and must opt up; DeepSeek's
server still maps `low`/`medium` to effective `high`. clud's `--effort` and
provider settings become initial values rather than locks. Subagent effort
follows the harness's own subagent policy instead of inheriting the removed
pin.

## DD-060: a foreign checkout's hooks run only after the user names it

**Status:** Accepted

**Context:** An `extern` root is a checkout clud cloned or the user granted,
never a repo the session root owns (#966 §6, #967 Phase 4). Before Phase 4 the
dispatcher ran only the parent's hooks, so no decision was needed about the
extern's *own* hooks. Once the dispatcher learned a checkout's own declarations
(DD-061, D4), running them unprompted would hand a repo the agent merely
visited control over every tool call — the same trust leap as running the
parent's hooks in the extern, which #841 already showed wedges sessions. But
the repo's hooks are also the point of the visit: a checkout with a guard
claud would silently skip is no safer than one whose guard never ran.

**Decision:**

- A gc-tracked extern checkout's Tier-B hooks are **off** until the user runs
  `clud extern trust <name>`. The first sighting of an extern root that
  declares hooks prints one visible notice naming exactly that command, then
  keeps the hooks off — no prompt flow, no silent skip:
  `[clud] extern checkout "dep" declares hooks, but is not trusted; they are
  not running. Trust it with: clud extern trust dep`.
- Trust is an allowlist in the parent's gitignored `.clud/settings.local.json`
  under `hook_trust.extern` (DD-062), keyed by checkout **name + origin URL**.
  The origin is read from the checkout's `.git/config` in-process — both
  remote section spellings, quoted values, and worktree `gitdir:` files — and
  a checkout with no readable origin matches by name alone.
- Re-cloning the same name from a different origin does **not** carry trust:
  the origin is half of the key. A stale entry left behind by gc teardown is
  harmless — it names a checkout that no longer exists, and re-cloning the
  same origin is still trusted.
- Roots the user named at launch — `--add-dir` targets and
  `permissions.additionalDirectories`, harvested into `CLUD_HOOK_ROOTS` — are
  registered as `extern` but are **never** trust-gated: naming them is the
  consent, exactly as naming a checkout is.
- `clud extern trust` with `--list` and `--revoke` round-trips the store; a
  trust entry is per-parent-repo, not global.

**Consequences:** A foreign checkout cannot execute hooks until its owner says
so, in the same repo where the session runs, with one command the notice
spells out. The gate costs nothing when the checkout declares no hooks (the
sighting check only runs for extern roots that have any). Trust is
machine-local and gitignored, so it neither leaks origin URLs into history nor
travels to other machines. Opted-in `.clud/hooks.json` declarations in an
extern are gated exactly like frontend-settings declarations — the trust
boundary is the checkout, not the file format.

## DD-061: child and extern hooks fire rooted at the declaring repo, layered parent-first

**Status:** Accepted

**Context:** #966 §6 fixes which repo's *own* hooks run in a nested repo.
Before Phase 4 the firing matrix covered only the parent's hooks: always in
the parent, always in a declared `child`, never in an `extern` (the #841
wedge). The matrix said nothing about the nested repo's own declarations —
the harness never loads them at all, because it reads hooks only from the
session root. A nested repo's guard therefore never ran unless the session
happened to start there.

**Decision:**

- **Parent root:** the parent's Tier-B hooks fire for touches to parent- and
  child-owned paths only — never for an extern-owned path alone.
- **Child root:** declaration is consent, so a child's own hooks run with no
  prompt, rooted at the child. Denial is layered: Tier A first, then the
  parent's hooks, then the child's own; any deny denies. A call that touches
  only child files still gets the parent's guards, and a call spanning roots
  fires each distinct root once, parent first.
- **Extern root:** only the checkout's own hooks, rooted at the checkout,
  trust-gated (DD-060). The parent's hooks never fire there.
- **Tier B source for sub-repos (D4):** the opted-in `.clud/hooks.json` is
  preferred; otherwise clud reads `.claude/settings.json`,
  `.claude/settings.local.json`, and `.codex/hooks.json` — the files the
  frontend itself would run there. Both the group shape (`hooks.<Event>` of
  `{matcher, hooks:[...]}`) and the legacy direct shapes parse, non-`command`
  handler types are skipped, `hooks.state` (codex's own trust table) is never
  an event, and duplicates across files dedupe by (event, matcher, command).
- **Codex sessions** gate child and extern Tier-B execution behind codex's own
  project trust: `codex_project_trusted` reads `[projects."<key>"]` in
  `~/.codex/config.toml`, where `<key>` is the normalized project path.
  clud detects a codex session by the absence of `CLAUDE_PROJECT_DIR` and does
  not run a repo's hooks there before codex itself would; the skip says so and
  names the fix.

**Consequences:** Every repo a session touches can be guarded by its own
hooks, rooted correctly regardless of where the agent stands, while the
parent's guards still cover everything it owns. Nested git repos are still
never auto-detected as children — declaration remains the consent that makes
the child tier's no-prompt trust sound. The parent's hooks and the extern's
hooks cannot both fire on one call, so there is no ambiguity about which
repo's guards apply in a foreign checkout.

## DD-062: trust state lives in the parent's gitignored `.clud/settings.local.json`

**Status:** Accepted

**Context:** DD-014 split `.clud/settings.json` (tracked — the repo's
declaration) from `.clud/settings.local.json` (gitignored — the user's
overrides). The extern trust allowlist (DD-060) is machine-local state with
no business in history: entries encode the user's decision about a specific
checkout and its origin URL. It must survive clud restarts, be written by
`clud extern trust` from any cwd under the parent repo, and be read on every
tool call by the hook binary.

**Decision:**

- The trust store is the `hook_trust.extern` key of `<repo_root>/.clud/
  settings.local.json`, an array of `{name, origin}` records. `record` and
  `revoke` are read-modify-write: every other key in the file is preserved,
  so the file's human owners keep using it for their own overrides.
- Parsing is lenient in both directions: a missing, empty, or corrupt store
  reads as an empty allowlist (a launch must not die on a damaged local
  file), and an unparsable store does not prevent a later successful write.
- Names are validated (no separators, no leading dots) before they are
  recorded; `is_trusted` matches origin exactly, or by name alone when the
  checkout has no origin.

**Consequences:** Trust is scoped to one machine and one parent repo, never
committed, and readable without spawning a subprocess — the hook binary reads
the store directly on every dispatch. The cost of the lenient read is
theoretical (a corrupt file silently distrusts everything, which is the safe
direction: hooks stay off). Because the store is per-parent-repo, cloning the
parent elsewhere starts untrusted, which is the conservative default.

## DD-063: `"auto"` relaxes `bash.block_cd` only for repos fully on clud hooks

**Status:** Accepted

**Context:** Phase 1 pinned every repo whose hooks were cwd-sensitive, with
`"auto"` resolving to strict-or-nothing from the raw frontend settings. Phase
2 made a repo's hooks dispatcher-managed once it opted into
`.clud/hooks.json`, which made the pinning unnecessary there: the dispatcher
roots every declared hook (cwd + `CLUD_PROJECT_DIR` = the declaring repo's
root, D10), so cwd drift no longer breaks them. Keeping such a repo strict
punished the migration — the exact behavior Phase 5 exists to remove. The
spec (D13) asks `"auto"` to be a three-level resolver whose relaxed level is
*earned*.

**Decision:**

- `"auto"` resolves to **strict** whenever any cwd-sensitive raw hook is in
  scope — `.claude/settings*.json`, `.codex/hooks.json`, or the user's home
  copies. The harness fires those hooks unrooted, so any drift breaks them;
  migration must not mask that.
- It resolves to **relaxed** when the repo is fully dispatcher-managed: a
  `.clud/hooks.json` opt-in (an empty file is not an opt-in) with no
  sensitive raw hooks. Relaxed denies only a `cd` whose resolved target
  escapes *every* registered root — the Phase 3 `CLUD_HOOK_ROOTS` set
  (parent, children, extern), not just the parent root — and allows all
  movement within them.
- It resolves to **off** when the repo has no hooks at all, or the session
  stands outside any repo.
- The scan records the opt-in as a `dispatcher_managed` flag; a declared
  hook's command text never counts toward sensitivity, because the
  dispatcher roots it. `"always"`/`"never"` stay as before.

**Consequences:** Migration is now a real upgrade — the repo earns
relaxation, and in-repo `cd`s stop being blocked. The strict level remains
the safe default for unmigrated repos, and a repo that keeps a sensitive raw
hook stays strict even after migrating, because that hook still fires
unrooted. The CwdChanged backstop (DD-064) is what makes relaxation
defensible: drift the scanner cannot see is detected reactively, warning
instead of blocking, because the relaxed invariant is no longer enforced at
PreToolUse alone.

## DD-064: cwd pinning and the CwdChanged backstop are hygiene, never correctness

**Status:** Accepted

**Context:** `bash.block_cd` blocks a session-mutating `cd` before it runs,
but the PreToolUse scanner sees only `cd`s written in a tool call. An alias
or a script that chdirs moves the session cwd invisibly, and nothing in the
pinning path can see it. The harness's `CwdChanged` event fires on every
directory change — including those — but the upstream cwd contract is
unstable (anthropics/claude-code#83636, #76708, #84685), the event carries no
decision control (exit 2 only shows stderr; the change is not reverted), and
it arrived only in Claude Code 2.1.83. The spec (D12) asks for the reactive
backstop, but the feature must not become load-bearing on any of that.

**Decision:**

- The `CwdChanged` handler resolves `bash.block_cd` against the session
  parent root (`CLAUDE_PROJECT_DIR`, which stays put while cwd drifts) and
  prints a hygiene warning when the new cwd violates the policy; it never
  blocks. A declared `CwdChanged` hook's exit-2 is downgraded to a warning,
  because a refusal cannot be enforced after the fact.
- The handler always exits 0, so no payload shape, hook failure, or harness
  misbehavior can turn it into a wall.
- The line is registered only where a bounded per-launch capability probe
  (`claude --version`, floor 2.1.83) says the installed client fires the
  event; any probe failure degrades silently to no line. It rides only on
  opted-in repos, so a non-opted-in launch keeps its pre-Phase-2 argv
  exactly.

**Consequences:** PreToolUse pinning remains the correctness layer; the
backstop is a diagnostic that makes relaxation (DD-063) safe to offer. A
frontend regression that breaks `CwdChanged` — or stops exporting
`CLAUDE_PROJECT_DIR` — costs users a missing warning, never a wedge. The
5-second probe adds nothing to launches without an opted-in repo, and a
hand-installed bare line still behaves (the handler is explicit-event-only
in the dispatch matrix).

---

## DD-065: PR CI waits are fail-fast, single-path, and cancel only the watched PR's own work

**Status:** Accepted

**Context:** Agents waiting on PR checks kept waiting for *all* matrix lanes —
the Mac builds being the long pole — even after a fast Linux lane had already
gone red, jamming the pipeline for the length of the slowest runner. Raw
waiter commands (`gh pr checks --watch`, `gh run watch`, `gh pr merge --auto`,
hand-rolled polling loops) wait locally, cannot see a first error, and never
release the queued lanes they no longer care about. The `git.pr_wait_fail_fast`
deny existed but defaulted off, missed `gh pr merge --auto`, and — in gated
(`tap`) sessions — the gate prefix hid the `gh` program name from the guard.

**Decision:**

- **One wait path.** The bundled `github/pr_merge_watch.py` tool is the only
  PR-wait primitive. The `git.pr_wait_fail_fast` deny now defaults **on** and
  covers `gh pr checks --watch`, `gh run watch`, `gh pr merge --auto`, and
  polling loops; the guard strips the `tap` gate prefix before matching.
  Explicit opt-out stays available.
- **Fail fast, always.** The watcher exits the moment a check is red and never
  idles out the rest of the matrix: with no branch protection, no allowlist,
  *or* protection that names zero checks, every check counts as required. It
  polls every 20 seconds by default so a watching agent reacts quickly. An
  empty check rollup — a fresh push whose run has not registered yet — reads
  as "no data yet" and keeps polling, never as green.
- **Break off, scoped.** On failure exit the watcher cancels only the watched
  PR's own remaining workflow runs on its head SHA — never another PR's jobs.
  The agent's reaction (fix + push) then supersedes the stale run through the
  CI workflow's existing `concurrency` group, which cancels the prior matrix
  for the same PR. No global or cross-PR cancellation exists anywhere.

**Consequences:** A red fast lane surfaces in ≤20s with the failing check
named, first-error classified, and the PR's remaining lanes released instead
of occupying runners for the slowest build. The escape hatch
(`CLUD_BAD_CMD_OVERRIDE`, or flipping `git.pr_wait_fail_fast` off) remains for
deliberate raw use. Merge readiness still requires all required checks green
and `mergeable=MERGEABLE` — fail-fast shortens failure, it does not weaken
the green gate.

## DD-066: clud reports an untrusted workspace and never records the trust itself

**Context (issue #1102):** Claude Code gates a project's
`.claude/settings*.json` behind a per-project trust decision stored as
`projects["<abs cwd>"].hasTrustDialogAccepted` in `~/.claude.json`. Until that
flag is set it loads none of that file and prints its own red banner saying so
— at the top of every one of up to 200 iterations of an unattended
`clud grind`, where it scrolls away unread.

It is easy to dismiss: clud already injects `--dangerously-skip-permissions`
([DD-002](#dd-002-yolo-mode-is-the-default-safe-is-the-opt-out)), so the
dropped `permissions.allow` entries change nothing for tool gating. Everything
else the file configures is a different story — the repo gets a materially
different unattended run than the one its author gets interactively, and the
only signal is a banner nobody is watching.

**Decision:**

- **Say it once, in clud's own voice, before iteration 1.** The notice lives
  in `main.rs` above the `launch_mode` match, outside both `run_plan_*` loops
  and covering the centralized-daemon path with the same call.
- **Only when it can matter.** Silent unless the run is multi-iteration, the
  backend is Claude, the state file is readable *and* says untrusted, and the
  repo actually ships a `.claude/settings*.json` for that decision to suppress.
  A bare directory gets nothing.
- **The iteration gate is the point.** On a single interactive launch the
  harness's own banner is on screen and readable; a second one would be a
  double banner in the one case that never needed help. The unattended
  multi-iteration run is the one with no other way to surface this.
- **An unreadable or unparseable `~/.claude.json` is `Unknown`, not
  untrusted.** A fresh machine, a relocated `CLAUDE_CONFIG_DIR`, and a
  half-written file must not tell a user their trusted workspace is untrusted.
- **clud never writes the flag, and the notice never coaches anyone into
  writing it by hand.** It points at the interactive prompt and stops there;
  a unit test asserts the text mentions neither `hasTrustDialogAccepted` nor
  `.claude.json`.

**Why not auto-trust?** Trust is the boundary that decides whether a
checkout's settings — including anything it declares that executes — are
honored. Accepting it on a user's behalf, in a tool whose premise is running
unattended in repos, makes clud the thing that disarms the check. The cost of
not doing it is one line of stderr per launch.

**The Codex asymmetry is deliberate, and worth naming.** clud *does* write
`[projects."<key>"] trust_level = "trusted"` into `~/.codex/config.toml`
(`hook_health/codex_trust.rs`, on by default via `auto_fix_hooks`). That is not
the same act: it is a hook-health *repair*, taken as part of installing clud's
own hooks into a project so they can run, reported in the repair output, and
opt-out-able with `--no-fix-hooks`. Flipping a Claude workspace to trusted to
silence an unrelated banner has none of that framing — the user asked clud to
run an agent, not to widen what a checkout is allowed to configure.

If a maintainer later wants the symmetric behavior, the shape already exists:
add a `RepairAction` alongside `AddCodexProjectTrust` so it lands under the
same reported, opt-out-able repair path. This decision is only that the
*notice* must not quietly become a write.

**Consequences:** An untrusted workspace with project settings is called out
once per launch instead of 200 times by someone else. A trusted workspace, a
non-Claude backend, a settings-free directory, and `--dry-run` all produce no
new output at all.

## DD-067: every clud-launched bash runs under `set -u`

**Status:** Accepted

**Context:** DD-056 closes with the one hole its own allowlist cannot reach:
"Redirections are performed by the shell, not the wrapper, so
`tap cmd > "$VAR/out"` can still write to a mis-expanded path... and `set -u`
in the session shell is the proportionate mitigation." That mitigation was
named but never shipped. The incident class from #1064 is broader than
redirections: `rm -rf "$SP"/` with an unset `$SP` expands to `rm -rf /` before
any hook, gate, or wrapper sees an argv worth objecting to. Every defense clud
has operates on command text or on the post-expansion argv; none of them can
object to an expansion that has already silently become nothing.

**Decision:** clud sets `BASH_ENV` in the backend child environment to a
generated file that runs `set -u`. Non-interactive bash — which is what every
Bash tool call is — sources that file before executing the command, so an
unset expansion aborts the shell with a named variable instead of proceeding
with an empty string.

The mechanism was verified rather than assumed, because a mechanism that
silently does nothing reads as coverage that is not there. Measured inside a
live tool call: `echo $-` gives `hmtBc` (no `i`, so non-interactive, so
`BASH_ENV` is read), and the same shell prints `[]` for `${SP}` without our
overrides and dies with `SP: unbound variable` with them.

Three details are load-bearing:

1. **The user's own `BASH_ENV` is chained, and sourced *first*.** Skipping our
   injection when theirs is present would be a silent coverage gap; clobbering
   it would break their setup. Sourcing theirs *after* `set -u` would be worse
   than either: stock startup files routinely test unset variables
   (`[ -z "$PS1" ]`, `$SSH_AUTH_SOCK`), so every tool call would die pointing
   at *their* file for a policy they never opted into. Ours goes last, which
   also means a `set +u` in their file cannot leave us disarmed.
2. **The escape hatch is env-only.** `CLUD_NO_BASH_NOUNSET=1` (also `true`,
   `yes`, `on`) launches without it. Deliberately not repo-configurable,
   matching the `CLUD_HOOK_DISPATCH` precedent: a repo should not be able to
   switch off a safety default for whoever runs an agent inside it.
3. **Both child-env paths carry it.** They were deliberate duplicates when
   this was written (#933), and the daemon copy had already drifted; #1209
   collapsed them onto one policy owner, `runner::apply_child_env_policy`, so
   a policy now reaches both by construction. The parity test stays: it is
   what would catch a future re-fork of the two paths.

**Consequences:** This is a real behavior change for every clud-launched
session, which is why it ships alone with its own revert story. A tool call
that relied on an unset variable expanding to nothing now fails loudly; that
is the point, but it will surface latent sloppiness in existing scripts as new
errors. clud ships no bash of its own for this to break — its hooks are Python
— but a user's bash-implemented skill or tool script is also sourced under the
same policy, since non-interactive bash reads `BASH_ENV` for script files too,
not only for `-c`. The generated file is written atomically and only when its
content differs, so concurrent launches cannot hand a shell a half-written
file and leave it silently unarmed.

---

## DD-068: `grind` delegates looping to the interactive harness

**Status:** Accepted. Its direct `/loop look at …` prompt is superseded by
DD-087; the ban on clud-side looping stands.

**Context:** `grind` was mistakenly implemented as an external clud loop:
clud added a completion-marker prompt, chose a fixed iteration count, and
relaunched the backend after each turn. That design made a harness-native
`/loop` request look like a request for clud orchestration. It also produced
headless and stream-rendered variants that are not a normal terminal session.
Issue #897 and PRs #950 and #1045 contain this obsolete guidance or preserve
parts of it. They do not define the current contract.

**Decision:** `clud grind` launches exactly one normal foreground interactive
PTY session and places a generated `/loop ...` request in that harness prompt.
The selected harness owns repetition, completion, blocked state, and the rest
of the session lifecycle. clud does not inject or poll DONE/BLOCKED markers,
apply an iteration cap, relaunch or re-prompt the harness, use headless
`-p`/`exec`, invoke the repeat worker, or enable stream-json rendering.

Only a harness that supports `/loop` in its normal interactive prompt supports
`grind`. A harness without that capability fails explicitly before launch;
clud must not substitute its own external loop. The full execution contract is
owned by [architecture/grind.md](architecture/grind.md).

**Rationale:** The interactive harness already owns the user experience and
knows how its `/loop` command should continue and stop. Adding a second owner
creates conflicting lifecycle rules, hides interactive controls, and causes
future maintenance to preserve stale clud behavior because tests and old plans
appear to require it.

**Consequences:** `clud loop` remains the distinct external-loop command and
retains its marker, artifact, iteration, and repeat semantics. Legacy `grind`
tests that expect markers, repeated launches, a 200-turn cap, headless argv,
or stream-json setup are specifications of the defect and must be replaced.
The replacement tests assert one PTY backend launch, a `/loop` prompt, and no
external loop state or renderer.

---

## DD-069: the recursive-delegation guard refuses depth 2 outright, and is opt-in

**Status:** Accepted

**Context:** #812 records a deep-research run that created 101 agents before
it was stopped, and an upstream `/code-review` that created 877 descendants
despite a five-level nesting ceiling (anthropics/claude-code#77361). The
bridge's `Task` block (#796) and `DEFAULT_MAX_CONCURRENCY` (1 at the time, 2
since #989) only observe the overload after the agents exist; they returned
`503 bridge busy` to requests that had already been created. Claude Code offers
no invocation-wide agent budget and no depth setting
(anthropics/claude-code#79953).

**Decision:** `clud-cmd-scan` gains a rule with no depth counter: a call to
`Agent` or `Workflow` whose `PreToolUse` payload carries an `agent_id` is
denied. The harness sets that field only for a call made inside a subagent, so
absence means the primary session and presence means a child. Effective
direct-delegation depth is 1. The guard is off unless
`CLUD_BLOCK_RECURSIVE_AGENTS` is set to an enabling value, and the denial names
the caller, the rule id `clud_agent_depth_exceeded`, and the switch.

**Rationale:** A ceiling bounds depth, but fan-out at each level is what makes
the total exponential, so any ceiling large enough to be useful is already
large enough to be ruinous — 877 descendants happened *under* one. Refusing the
second level is the only bound that does not depend on the branching factor.
It lives in the existing hook rather than a per-launch injected settings
document because clud already runs that hook on every tool call under a `*`
matcher, and a second hook would be a second thing to trust, migrate, and keep
in step (DD-039's lesson about two installers). Opt-in for the same reason
`shell.disable_powershell` shipped off: denying agent creation changes what an
agent may do, and a wrong default breaks legitimate orchestration silently.

**Consequences:** Only tool-visible creation is bounded. A workflow script's
internal `agent()` calls emit no blocking event and remain uncontrolled; the
docs say so plainly, because an operator who believes fan-out is capped will
not go looking when it is not. A `Bash`-only matcher never delivers an `Agent`
event, so the README's `*` matcher is a requirement, not a suggestion. An
empty `agent_id` reads as the primary session, so a harness that emits `""`
cannot lock the top-level session out of delegating.

## DD-070: interactive Codex runs through the PTY pump on Linux/macOS and inherits the console on Windows

**Status:** Accepted; the Windows exception is superseded by DD-086.

**Context:** #1181. Codex's TUI writes every history line with an explicit
`\r\n`, but its `/goal` status cell hands the whole objective to one span
(`codex-rs/tui/src/goal_display.rs::goal_usage_summary`), so a pasted
multi-line objective reaches the terminal with bare `\n` while the terminal is
in raw mode with `OPOST` off. Each line starts where the previous one ended.
The Windows console masks this with its LF→CRLF output processing; Linux and
macOS terminals do not. Present in codex 0.154.0 and at upstream HEAD, and it
reproduces with plain `codex`, so clud is not the cause.

clud could not mask it either: since PR #47 the rule was "Codex with a parent
TTY runs as a subprocess inheriting the terminal", so no Codex bytes passed
through the pump. That rule was written because the old crossterm event loop
dropped Codex's startup `\x1b[6n` reply and Codex hung. The same PR replaced
that loop with the raw byte pump, which forwards query replies verbatim, so
the hang's mechanism was already gone. (The PR is titled `(#46)`, but #46 is
an unrelated CI issue; see #737.)

**Decision:** `resolve_launch_mode` picks PTY for interactive Codex on every
platform except Windows-with-a-TTY, which keeps subprocess. The pump chains a
stream-resumable bare-LF→CRLF filter (`codex_lf::CodexLfNormalizer`) after
the OSC title stripper, enabled only when the backend is Codex. `--subprocess`
and `--pty` still override the rule on every platform.

**Rationale:** Being in the byte path is the only way clud can give
Linux/macOS what the Windows console gives for free. Windows stays on
subprocess because ConPTY adds its own repaint layer (#515) and the console
already masks the bug, so wrapping there costs and gains nothing. The filter
is Codex-gated rather than global because a TUI may legitimately emit a bare
LF inside a scroll region and rely on the column staying put; Codex never does
in its own writes. Masking in clud rather than waiting for upstream because
until Codex ships the one-line fix every Linux/macOS user hits this on every
multi-line `/goal`.

**Consequences:** PTY mode also turns on clud's PTY-only features for Codex on
Linux/macOS (drag-drop path normalization, Ctrl+V image paste, F3 voice) and
the pump's 5 ms idle stdin poll (#691's cost table). The Windows guard is
`backend::tests::test_codex_interactive_with_tty_uses_subprocess_on_windows`,
which runs on the Windows exec lane; its Linux/macOS twin asserts PTY. Python
`--dry-run` tests are unaffected because they run without a TTY, where Codex
was already PTY.

## DD-071: toasts are composited inside the terminal stream, not drawn in an external window

**Status:** Accepted

**Context:** #1189. clud's mid-session messages (the CPU banner) were
`eprintln!`ed onto the terminal a harness TUI was drawing on, with no
positioning, erasure, expiry or serialisation against the child's redraws, and
visibly corrupted Claude Code's footer. The ask was a semi-transparent toast,
anchored to the terminal, with a close button, that expires when no longer
relevant.

Two families were researched. **External overlay windows** (a layer-shell or
X11 surface positioned over the terminal window) work on KDE, Hyprland, Sway,
niri, COSMIC and X11, but need a geometry adapter per compositor (Hyprland and
Sway only by polling), cannot tell a single-instance terminal's windows apart
without title tricks (all kitty OS windows share one pid and class on KWin),
float over unrelated apps unless occlusion is tracked, need a Shell extension
on GNOME, and cannot work over SSH. **Compositing into the PTY stream** needs
clud to own the child's bytes (PTY mode), but is anchored by construction on
every desktop and over SSH.

**Decision:** Toasts are composited by the PTY pump's writer thread in tiers:
kitty graphics where the terminal fully supports it (kitty, Ghostty, WezTerm),
styled text cells on the alternate screen elsewhere, and a fallback surface
otherwise — Claude Code's own `statusLine` (clud composes one that chains the
user's existing status line) or the terminal title. Subprocess-mode Claude
launches use the status line. Sixel is not used. The external overlay window
is deferred.

**Rationale:** kitty graphics blend real alpha over text on a separate layer,
so removing a toast is deleting a placement — no repaint, no divergence from
the child's text model. Sixel has 1-bit transparency and replaces cells, so it
costs the same repaint as a text toast while only adding looks. Text cells are
restricted to the alternate screen because on the main screen a scroll pushes
them into the terminal's native scrollback permanently. The status line is the
only anchored surface Claude Code offers to an external process, and chaining
preserves the user's own line. clud never enables mouse tracking, because that
takes selection and wheel scrollback away from inline TUIs; the close button
is clickable only when the child already reports SGR mouse events.

**Consequences:**

- With toasts enabled, every Claude launch carries a launch-scoped
  `--settings` source for the `statusLine`. This deliberately breaks the
  earlier "a repo that has not opted in sees an identical launch" rule
  (`clud_hooks_compile.rs`); `[foreground.toasts] claude_statusline = false`
  restores the old argv.
- In PTY mode every child byte also feeds a `vt100` shadow and an escape
  tracker while toasts are enabled.
- Claude on Linux/macOS sees in-grid toasts only in PTY mode; since DD-086
  that is its interactive default, as it is for Codex (DD-070).
- Konsole and iTerm2 stay on text cells until their kitty graphics support is
  validated; `CLUD_TOAST_TIER` overrides the choice.
- Subprocess-mode Codex (Windows) has no toast surface.

## DD-072: server-side settings are one baked-in JSON document with per-section last-known-good

**Status:** Accepted

**Context:** #1192. DeepSeek renames API model names in place.
`deepseek-v4-flash` became `deepseek-flash` with V4.1-Flash. clud compiled the
DeepSeek names into the binary, so following a rename required a clud release
and an upgrade. The first draft served a DeepSeek-only file. Review asked for a
general mechanism instead: settings baked into clud, plus robustness, so a
malformed server copy never breaks a client and clients carry on with what they
last had.

**Decision:** `crates/clud-bin/assets/server-settings.json` holds a
`schema_version` and a set of independently validated `sections`. The build
embeds the file as the built-in copy, and installed builds fetch the same path
from `main`.

- **Resolution.** Each section takes the first valid value among the copy served
  now, the last cached valid copy, and the built-in copy.
- **Section errors.** A present but invalid section keeps its last good value.
  An absent or `null` section resets to built-in.
- **Document errors.** A whole-document failure (bad JSON, a root that is not an
  object, an unsupported `schema_version`, `sections` missing) leaves the cache
  untouched.
- **Refresh.** A cache younger than 15 minutes is used without a request.
  Otherwise a background thread fetches, merges, and writes the cache
  atomically. The launch waits for it for at most 750 ms. After a failed
  attempt, clud does not retry for 15 minutes.
- **Parsing.** Strict: no duplicate keys, no trailing content, UTF-8 only,
  64 KiB cap, and nesting depth bounded.

**Rationale:**

- **Isolation per section.** One malformed setting cannot freeze or break
  another. [Firefox Remote Settings](https://firefox-source-docs.mozilla.org/services/settings/index.html)
  isolates collections for the same reason.
- **Last-known-good over rollback.** A broken edit on `main` leaves clients on
  the value they last validated, much like Chromium's variations "safe seed". A
  deliberate reset is still possible by removing the section.
- **One file for both copies.** The built-in copy cannot drift from the served
  one, and guard tests reject a document that is not strict JSON or that lacks
  a valid value for a registered section. So a broken edit fails CI before it
  can reach `main`.
- **Bounded latency.** On a healthy network an edit applies within one launch.
  Offline, the cost is at most 750 ms per 15 minutes.
- **Strict parsing.** `serde_json` silently keeps the last of duplicate keys,
  which is the wrong answer for hand-edited configuration served to every
  install.

**Alternatives rejected:**

- **One typed struct for the whole file.** One bad field would invalidate every
  setting.
- **One file per setting.** It costs N requests per refresh and allows partial
  updates across files.
- **A signed manifest with per-section blobs.** It needs key management and a
  publishing pipeline, which is disproportionate for a few settings served over
  TLS from this repository. Deferred.
- **A synchronous fetch on first read.** It adds up to 2 s to a launch whenever
  the cache is stale.
- **Pure stale-while-revalidate.** Edits land one launch late, and short-lived
  commands can exit before the refresh finishes.
- **A daemon-owned refresh.** The daemon is not always running, and #542 asks
  that the daemon not grow new fixed-interval work.
- **A jsDelivr mirror.** It caches branches for 12 hours with no purge, so it
  can serve an older document than the local cache and roll values back.
- **ETag revalidation.** The document is under 1 KiB, so a 304 saves nothing
  measurable. Deferred.

**Consequences:**

- **Changes ship on merge, not release.** An edit to the JSON on `main` reaches
  installed builds within about 20 minutes (a 5-minute CDN cache plus 15 minutes
  of local freshness).
- **Adding a setting** takes a `Section` type, one `SECTIONS` entry, and its
  built-in value. The guard tests fail until all three agree.
- **Unknown sections are kept in the cache**, so a newer clud sharing
  `~/.clud/cache` still sees them. Bumping `schema_version` makes older builds
  ignore the whole document, so additive changes must not bump it.
- **Controls:** `CLUD_SERVER_SETTINGS=0` uses the built-in copy only;
  `CLUD_SERVER_SETTINGS_URL` points at a draft and bypasses the cache;
  `CLUD_VERBOSE_SERVER_SETTINGS=1` explains fallbacks.
- **Tests stay offline.** Library unit tests never fetch, and the subprocess
  test harnesses set `CLUD_SERVER_SETTINGS=0`.
- **A served DeepSeek default is visible**: `--dry-run` reports it as
  `model_source: server_default`.

## DD-073: every inline selector renders through one component

**Status:** Accepted

**Context:** #1195. clud has three inline terminal selectors: the launch-setup
scope prompt, the bare-launch harness picker, and `clud settings`. Each owned
its own copy of the terminal plumbing:

- the raw-mode guard and cursor hide/show;
- draining pending input;
- key decoding;
- redraw arithmetic;
- line endings.

#1063 found that `writeln!` under raw mode walks diagonally on Linux and macOS,
because raw mode clears `OPOST` and a bare `\n` stops returning to column
zero. #1106 fixed only the scope prompt's copy, so the harness picker (#943)
and `clud settings` still shipped the bug. The copies had drifted in other ways
too:

- Only the picker ignored key releases, which crossterm reports as separate
  events on Windows.
- Each copy kept a hand-maintained line count for its cursor-up redraw.
- None of them counted rows that wrap at the terminal width.

**Decision:** `crates/clud-bin/src/selector.rs` owns all terminal I/O for inline
selectors:

- raw mode and cursor hide/show;
- draining pending input;
- one key decoder that ignores releases;
- the tick loop;
- CRLF-only rendering from a declarative `View`;
- physical row counts that include wraps;
- redraw and erase;
- a keep-or-erase exit.

A selector implements `Selector`: a view, a key handler, an optional tick, and
its exit style. The settings save prompt is a mode of the settings menu rather
than a second key loop.

**Rationale:**

- **Fixed once.** A rendering or input fix lands once and reaches every
  selector.
- **Derived row counts.** They come from the rendered frame rather than being
  maintained next to it, so a new row or a long wrapped note cannot desync the
  redraw.
- **Testable.** Terminal behaviour runs against a scripted terminal and a fake
  clock: countdown redraws, redraw arithmetic, and save-prompt flow.
- **Guarded.** A compile-time test forbids the migrated modules from enabling
  raw mode, reading events, or writing escape sequences themselves.

**Alternatives rejected:**

- **CRLF edits in each file.** That is the fix that already regressed.
- **A shared line-writer helper only.** The key loops and redraw arithmetic
  would keep diverging.
- **A TUI crate (`inquire`, `dialoguer`, `ratatui`).** A heavy dependency with a
  different look, usually an alternate screen, no countdown or value-cycling
  UX, and extra cross-build and wheel cost.

**Consequences:**

- A new inline selector implements `selector::Selector` and is added to
  `migrated_selectors_never_drive_the_terminal_themselves`.
- Hint, footer and note indentation is uniform. The `clud settings` save prompt
  is now indented like other footer lines.
- Ctrl-C and Ctrl-D close the frame the same way a normal exit does, then return
  `Interrupted`.

## DD-074: rm uses a PATH identity backstop and a post-expansion shim

Issue #1183's owner authorized validating executable lookup on the effective
PATH. The hook compares actual bytes to the packaged shim, while the executable
validates expanded operands. Neither layer replaces the other: expansion loses
source provenance and the executable cannot intercept non-rm deletions.

Real removal deliberately requires CI plus detected Docker. Unit-test builds
exclude the real executor. See [rm protection](architecture/rm-protection.md) for
the contract, supported platforms, retirement rationale and residual boundary.

---

## DD-075: Codex-via-Claude owns the workflow role aliases

**Context:** Claude Code workflows and subagents may request the built-in
`opus` and `sonnet` aliases even when `clud --codex --harness claude` launched
the session. The gateway cannot serve those Anthropic IDs. Before this decision,
the translator treated both as an unknown `claude*` ID and substituted the
launch default, collapsing workflows that deliberately used Opus for planning
and review and Sonnet for implementation onto one Codex tier.

**Decision:** The direct Codex-through-Claude child overlay removes ambient
role-alias settings and sets `ANTHROPIC_DEFAULT_OPUS_MODEL` to the advertised
Codex Sol discovery ID and `ANTHROPIC_DEFAULT_SONNET_MODEL` to the advertised
Codex Terra discovery ID. It also sets their display names to honest Codex
labels. Haiku is left harness-owned. The override does not apply to native
Claude or the unified gateway.

**Rationale:** This changes an upstream role request before it reaches the
bridge, preserving intended tier separation and making the picker labels match
what will be billed. It is narrower and more transparent than teaching the
translator to guess whether every `claude*` request came from a workflow,
subagent, or a user picker selection.

**Consequences:** Direct bridge sessions now map Opus-role work to Sol and
Sonnet-role work to Terra. The general picker limitation in DD-054 remains:
discovery cannot remove built-in Anthropic rows, and a user can still select
one. The aliases make that normal workflow path correct; they do not turn the
gateway into an Anthropic provider.

---

## DD-076: daemon worker environments start from the OS login baseline

**Context:** A daemon outlives the terminal that started it. Inheriting that
first terminal's environment made every later worker depend on an accident of
auto-start ordering: an activated virtual environment, transient toolchain
shim, or stale `PATH` could survive for days after the initiating shell exited.
The client overlay from #1157 fixes values the client sends, but cannot remove a
stale value that the current client does not contain.

**Decision:** The daemon materializes a standard login environment at startup,
refreshes it on every session admission plus a five-minute idle backstop, and
persists that exact base in each `WorkerLaunchSpec`. POSIX obtains it from a
bounded, clear-environment login-shell evaluation; Windows obtains it from the
machine and user environment registry keys, with normal `PATH` composition and
expand-string handling. Daemon-private protocol/lifecycle values layer above
that base; the initiating client's environment layers above both.

**Rationale:** The OS/login layer is deterministic for an account and excludes
an arbitrary session's activated state. Capturing it in the worker spec means a
new refresh safely affects only future workers, while older serialized specs
keep their legacy fallback during rolling upgrades. A `PATH` union would retain
a stale prefix and reproduce the original shadowing problem, so client `PATH`
is replacement-only.

**Consequences:** A login profile that fails or exceeds the bounded evaluation
time does not block sessions: the last known-good baseline remains in use.
Session-specific exports must travel in the client environment; clud does not
try to infer them from the daemon. Durable API session records still lack a
client environment and remain on their separately documented compatibility
path. The full contract is in [daemon environment](architecture/daemon-environment.md).

---

## DD-077: a launch-time model pin constrains every model slot, not just the main model

**Context:** #1257 amended [DD-054](#dd-054-the-model-picker-belongs-to-the-harness-and-discovery-only-adds-rows)
in the failure direction: `--model` only replaced `ANTHROPIC_MODEL`, so a
launch pinned to a non-Anthropic model still handed the harness independent
Fable/Opus/Sonnet/Haiku/subagent role mappings and an open gateway-discovery
catalog. The repro `clud --openrouter --model xiaomi/mimo-v2.6-flash` bounded
the main conversation, then quietly reached Claude through subagent and
discovery-picked rows the user believed were pinned away.

**Decision:** Every launch derives a model allowlist. Precedence:
`--allow-model` (repeatable; replaces rather than extends everything) ->
`--model` -> the previous model selection's wire id (the id `--dry-run`
reports as `model_selection`), which is the *only* pin when the user named no
model -- announced exactly once as a green
`[clud] info: no --model given; pinned to previous model selection: <id>`
startup line, green only on a TTY, plain text otherwise. Membership is
case-insensitive identity or same catalog row across the CLI, wire, discovery,
and legacy-alias namespaces; effort and `[1m]` context suffixes never change
which row was meant. Under a pin: a direct Anthropic-compat overlay sends
every role slot the pinned wire id and unsets
`CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY` (a startup notice says so), with
an ambient `CLAUDE_CODE_SUBAGENT_MODEL` winning the subagent slot only when
it is itself inside the allowlist; the unified gateway pins slots to the
pinned id's discovery id and keeps discovery on but filters `/v1/models` to
the allowlist, because clud proxies that catalog; the codex-via-claude bridge
folds its own DD-038 opus/sonnet role rows into the boundary so clud never
refuses its own configuration. The bridge refuses an out-of-allowlist model
with `400 invalid_request_error` naming the allowed set and logs
`model_not_allowed`, exempting ids that contain `haiku` (the harness's
side-model machinery) or start with `claude` (the DD-038 substitution and the
caller's own Claude credential) -- the exemptions are per-request and do not
lift the main-model pin. An explicit allowlist outside which the resolved
selection falls fails at launch with exit code 2, before bootstrap. No pin
and no allowlist means an empty allowlist, and every consumer is
byte-for-byte the pre-#1257 behavior. `--dry-run` exposes `allowed_models`
and `pinned_from_previous_selection` so the boundary is auditable without a
paid request.

**Rationale:** An inherited pin is still a cost boundary the user is relying
on; a pin covering only the main model is not a boundary at all, because one
subagent turn or one `/model` pick escapes it. Reusing the resolved
selection as the default pin means a normal `clud --openrouter` session is
exactly as constrained as its own `--model` would be, with no new flag to
remember and no second picker to maintain -- discovery still only adds rows,
and the harness still owns the picker.

**Consequences:** DD-054 is amended, not overturned: discovery and `/model`
belong to the harness and still only add rows, but a constrained direct
launch asks for no discovery at all and announces that, while the picker's
built-in Anthropic rows remain reachable there through the bridge exemptions
for side-model traffic. A gateway pick outside a unified launch's boundary is
refused at the bridge with the allowlist in the message rather than silently
relabelled. Only the inherited pin is announced, because an explicit pin is
what the user just typed. The full slot contract is in
[provider-selection.md](architecture/provider-selection.md#openrouter-model-selection-contract).
## DD-078: OpenRouter context windows ride server-settings, refreshed from the provider's datasheet

**Context:** Claude Code clamps auto-compact to 200k for any model its catalog
does not describe, and clud only emits a window variable on an exact
static-catalog hit. Live OpenRouter inventory is deliberately absent from that
catalog (DD-054), so every newly listed OpenRouter model — starting with
`xiaomi/mimo-v2.6-flash`, whose real window is 1M — compacted far too early
(#1258). The fix needs exact windows for models clud has never heard of, kept
current without a release, from a config file stored in this repo that
downloads on its own.

**Decision:** Fold OpenRouter's public datasheet
(`GET https://openrouter.ai/api/v1/models`, `context_length` per row) into the
server-settings document (DD-072) as a `model_contexts` section — a flat
`{wire-id: tokens}` map — populated by a scheduled producer
(`ci/refresh_model_contexts.py`, run daily by the repository's first cron
workflow) that rewrites only its own section and exits non-zero on fetch or
parse failures. At launch, the Anthropic-compat overlay sets
`CLAUDE_CODE_MAX_CONTEXT_TOKENS` from that map when the catalog has no
reviewed `claude_max_context_tokens` for the wire ID: catalog wins, ambient
user values win, absent-in-both emits nothing.

**Rationale:** The fetch, cache, last-known-good, and per-section validation
machinery already exists and is the repo's single source for "changeable
without a release"; a sibling document would fork that lifecycle and create a
second owner per fact. Exact integers beat the harness's lossy `[1m]` boolean,
and teaching the harness the real window beats
`CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT`, which only restores
wait-for-the-API failure at the limit. Catalog-wins keeps the reviewed goldens
(DeepSeek 786432, Kimi 1048576, Codex 1050000) authoritative.

**Consequences:** A new OpenRouter model's window reaches installed builds in
roughly twenty minutes after listing, with no release and no catalog edit,
preserving DD-054's boundary. A datasheet shape change turns the daily run red
for review instead of silently shrinking the map. The bounds are mirrored in
the producer and in `ModelContexts::validate`; changing one without the other
turns CI red.

## DD-079: A hung model request is reported where clud can see it, and is never mistaken for a clean end

**Context:** zackees/clud#1263. A direct `--openrouter --model
xiaomi/mimo-v2.6-flash --effort low` launch stalled for 4m39s with zero
assistant output: no error, no timeout, no retry, no spinner state in the
transcript, clud's logs, or the daemon events. The session looked dead and the
user interrupted it; the next turn answered in ~4s, so the API was healthy.
Auto-compaction was healthy and `totalAPIDuration` minus `withoutRetries`
differed by ~0.3s across a 112-minute session, so this was neither context
exhaustion nor a retry storm.

Three independent gaps produced it, and they are not the same gap:

1. **The direct route has no clud-side watchdog at all.** `--openrouter`
   conflicts with `--unified`, so routing is Direct and
   `apply_anthropic_compat_overlay` points the harness straight at
   `https://openrouter.ai/api`. clud is not in the request path and cannot see
   a byte of the stream. Every other overlay pushed `API_TIMEOUT_MS`
   (3 000 000 ms); this one did not, so a direct launch inherited the harness's
   undocumented client timeout and nothing said so.
2. **The bridge answered a timeout with the wrong status.** A transport
   timeout on the proxy hop returned 502 `api_error` — "this route is dead" —
   while 504 `timeout_error` ("retry me") already existed for every other
   timeout the bridge raises.
3. **A mid-stream failure was indistinguishable from success.** The proxy's
   read loop treated `Ok(0)` and `Err(_)` alike and then wrote the terminating
   chunk, so a truncated turn reached the client as a normally-finished one.

**Decision:** Fix each where it lives, and do not invent a fourth mechanism.

- `apply_anthropic_compat_overlay` pushes `API_TIMEOUT_MS` as a *default*, so
  all routes agree and an ambient user value still wins (DD-059's precedence),
  and the launch emits a notice naming the route and the effective value. This
  is the whole of what is available on the direct route: the only lever there
  is the harness's own client timeout, and clud can at least set it, log it,
  and say plainly that it cannot detect a hang on that path.
- The proxy hop distinguishes a timeout from a dead peer (`transport_timed_out`
  walks the `ureq` error chain for an `io::ErrorKind::TimedOut`, which is what
  a `WouldBlock` socket timeout normalizes to) and answers 504 for the former.
- The proxy hop's budget becomes genuinely byte-idle (see the DD-028
  amendment), and its read loop separates a clean `Ok(0)` EOF from an `Err`,
  reporting the latter as a sanitized in-band SSE `error` frame and logging it.

**Rationale:** The detector has to key on *received-byte idleness*, never on
turn duration. A model thinking for minutes is indistinguishable from a hang by
wall-clock alone — and this model thinks for minutes routinely — so a
turn-duration timeout would kill legitimate work. A healthy long think keeps
resetting an idle timer because it keeps emitting deltas; a dead connection
trips it. This is the shape DD-028 already specifies and the shape the reported
hang's own evidence supports: during the 4m39s window the transcript held zero
assistant records of any kind, not a partial or aborted one, which is a quiet
stream rather than slow generation.

**Consequences:** A stalled turn on a bridged route now ends in a named error
instead of a silent truncation, and a long healthy stream is no longer cut off
for outlasting its budget. The direct route still cannot be *detected* by clud
— that is a property of the routing mode, not a gap left open — so the honest
deliverable there is a known, logged timeout plus a notice saying clud is not
watching. Note also that the harness's own client path shows no spinner, error,
or retry state for a hung request; that remains an upstream gap, filed
separately.

**Superseded for descriptor-backed direct routes by DD-081:** The bridge's
3 000 000 ms API request budget remains unchanged; the direct route no longer
copies it merely to make the overlay values agree.

## DD-080: Codex updates use one verified subprocess, not a removal-shim exception

**Context:** zackees/clud#1271. The standalone Codex installer removes staging,
stale links, and temporary downloads while updating. Inside a CLUD-managed
session, those calls resolve to the #1183 removal shim and fail its CI plus
Docker gate, aborting the install before extraction.

**Decision:** Expose `clud codex-update` on Linux and reuse it for automatic
Codex bootstrap. Fetch only the fixed official release URL, reject redirects,
cap the response, and require an audited SHA-256. Execute that script with a
minimal child environment with OS-owned tools first; preserve only an existing
`~/.local/bin` PATH entry after them so profile handling remains faithful. Do
not alter the session PATH or the removal shim's gate. The child may remove its own installer staging
as the verified script directs. Arbitrary shell commands cannot supply script
contents, an install path, or deletion operands to this entry point.

**Rationale:** A shell receiving installer text from a pipe cannot prove the
text's origin to the removal shim. Parent-process ancestry or a user-set
environment token would grant the same exception to unrelated commands. A
deliberate update command gives CLUD a trust boundary it can verify before the
script executes, while the common session shell remains protected.

**Consequences:** The normal pipe-to-shell installer remains denied inside a
managed session; users run `clud codex-update` instead. Upstream installer
changes require a reviewed hash-pin update. A fixture exercises fresh,
already-complete, and stale-staging paths, and the normal gate is regression
tested for a nonexistent operand without CI evidence.

## DD-081: Direct routes do not lengthen Claude Code's API request timeout

**Context:** zackees/clud#1270. DD-079's bridge-era `API_TIMEOUT_MS=3000000`
(50 minutes) was copied to descriptor-backed direct routes without a direct-
route rationale. Claude Code documents a 600000 ms (10-minute) per-request
default. Current Claude Code versions also have separate streaming idle and
first-byte watchdogs, whose availability and defaults have changed across
versions. The API request timeout is not a clud-side stream-idle detector.

**Decision:** Set the direct-route `DIRECT_API_TIMEOUT_MS` to `600000`, while
retaining the bridge/unified `BRIDGE_API_TIMEOUT_MS=3000000` from DD-028.
Keep `push_default`, so an ambient `API_TIMEOUT_MS` wins unchanged.
The direct-route launch notice reports the effective value as a *Claude Code
API timeout* and says only that clud does not monitor this request. Do not
imply that a 10-minute timer detects every stalled stream or terminates a
whole agent turn. Stream watchdog behavior remains owned by the installed
Claude Code version; clud's bridge idle detector is independent of this value.

**Rationale:** A 50-minute pre-response stall was much longer than the
documented client default without any evidence that legitimate individual
requests require it. Matching the documented default restores the upstream
request budget while retaining an explicit, testable direct-route value.
For a request phase covered by `API_TIMEOUT_MS`, Claude Code surfaces its
timeout/error (and may retry according to its own policy); clud does not
observe or recover the direct request. A stalled streaming response is governed
by Claude Code's separate watchdogs, not guaranteed by this setting.

**Consequences:** Without an override, the direct notice changes from
`API timeout 3000000 ms; a hung request cannot be detected by clud on this
route -- set API_TIMEOUT_MS to override` to
`Claude Code API timeout 600000 ms (set API_TIMEOUT_MS to override); clud does
not monitor this request`. An explicit ambient value is still reported and
preserved. Bridge and unified route defaults do not change. The future
direct-route listener in #1263 remains the route for clud-owned per-request
progress and recovery.

## DD-082: Provider credentials need a verdict, not merely a vault record

**Context:** zackees/clud#1269. The presence-only preflight from #878 can
accept a corrupted vault value, launch a healthy-looking direct session, and
leave Claude Code to retry a provider 401 repeatedly. The last-four mask alone
cannot distinguish two otherwise different keys with the same suffix.

**Decision:** For descriptor-backed Anthropic-compatible providers, check the
stored value's shape locally and perform one bounded authenticated GET against
a provider-owned, non-generation endpoint on each live launch and auth-status
request. Treat 401/403 as rejected and stop a launch with exit 2; treat local
shape violations as malformed without making a request. Timeouts, connection
errors, redirects, and other inconclusive responses warn once and fail open.
Keep dry runs vault- and network-free. Preserve the native-vault boundary and
never put a key in a probe diagnostic. Report the provider's
message only after masking credential-shaped text and control characters;
include the stored value's length and short digest alongside its last-four
mask. Both command-line and interactive entry reject malformed values before
writing them. The interactive prompt masks accepted characters with asterisks
and supports Backspace, replacing #878's completely hidden input.

**Rationale:** A local CLI must remain usable offline, but a definitive auth
rejection should not waste a full session. A cheap provider-owned GET avoids
generation cost. The fingerprint is diagnostic only; it lets a user compare
what clud stored with what they intended without disclosing the key.

**Consequences:** Live launches can spend up to the configured probe budget
before the harness starts. Revocation is noticed on the next launch rather
than hidden behind a verdict cache. A valid 200 leaves the child env, argv,
and startup notices unchanged. Provider status can now distinguish missing,
malformed, rejected, and configured credentials.

## DD-083: Correct exact CLUD option spelling, reject near misses before passthrough

**Context:** zackees/clud#1268. The pre-split CLI grammar sent mistyped
DeepSeek selectors and nearby API keys to the backend as prompt text. Clap
could not suggest a correction because it never saw the unknown option.

**Decision:** In the top-level option region only, correct the exact
`-deepseek` alias and Unicode-dash spellings of public CLUD long options
before inline-key extraction. Probe unmatched flag names against clap's
top-level options individually. A near miss is an exit-2 error; an unknown
option followed by a key-shaped token also fails closed. Unrelated backend
flags continue byte-for-byte, and `--` remains the explicit escape hatch.
Bare-word near misses only emit a note because they may be prompts. Keep
execution argv intact, but share one key masker at launch-record, dry-run,
crash-report, and diagnostic output boundaries.

**Rationale:** Exact correction preserves intent without fuzzy routing;
clap's own suggestion remains the authority for uncertain flags. Output
redaction protects CLUD-owned surfaces without breaking web-terminal child
forwarding. Inline keys remain visible in shell history and process argv,
so native-vault entry is the safer credential path.

## DD-084: Transcript usage is the direct-route cumulative fallback

**Context:** zackees/clud#1267 reverses #1227's refusal to derive cumulative
tokens from Claude's undocumented JSONL transcript. Claude's documented
status-line callback exposes only the last request's token triple, and the
direct Claude, DeepSeek, Kimi, and OpenRouter routes do not have a bridge
ledger. A per-call number displayed beside an aggregate label is misleading.

**Decision:** Keep bridge-observed provider totals authoritative. Otherwise,
incrementally read the main transcript and sibling subagent JSONL, accepting
only complete integer usage objects with a response id. Deduplicate globally
across files and replays using session-lifetime SHA-256 id hashes; retain exact
cursors and hashes until the launch ends rather than evicting old ids. Store
the cumulative snapshot separately from the parent-owned toast file, guard
updates with a per-launch file lock, and atomically replace the snapshot. Both
Claude's status line and the PTY compositor read it. An absent or malformed
transcript never makes last-call counters masquerade as cumulative totals.

**Rationale:** Transcript usage is the harness's own record of provider counts,
not a tokenizer estimate. Hashed ids and paths enable replay-safe accounting
without persisting prompts, raw session ids, or agent ids. The status-line
callback already receives the transcript path, so direct routes need no
listener in the request path. Native Codex without that callback still needs
bridge-observed usage to display a triple.
## DD-085: Keep OpenRouter's changing price inventory separate from the model catalog

**Context:** zackees/clud#1256. OpenRouter's model inventory and token prices
change independently of clud releases, while the reviewed provider catalog
and harness-owned picker have separate compatibility contracts (DD-054).

**Decision:** Publish one additive, versioned JSON document from a scheduled
producer. It records normalized model rows, explicit text/tool/context/price
eligibility, and a convenience shortlist ranked by a 70% input, 20% output,
10% cached-input estimate. The consumer fetches only the fixed raw GitHub
document with redirects disabled and a bounded timeout, caches valid data
under daemon state, and falls back through the last good copy to an embedded
seed. Unknown fields remain compatible; the static catalog and picker do not
change.

**Rationale:** A price ranking is useful only when its population and token
mix are visible. Calling the results the lowest-priced *eligible* rows avoids
implying that provider metadata measures programming quality. Raw rows let a
later build apply another weighting without scraping OpenRouter at launch.

**Consequences:** Prices may be six hours stale on a healthy install, or older
when fetches keep failing. The producer fails visibly on invalid upstream data
and commits only validated deterministic output.

## DD-086: every console launch runs through the PTY pump; subprocess is for headless and redirected output

**Status:** Accepted. Supersedes DD-070's Windows exception.

**Context:** zackees/clud#691. Claude defaulted to subprocess since `a4ef5f5`
with no recorded rationale; every Claude-specific reason cited in code was
stale or mis-cited (#737). Codex kept subprocess on Windows (DD-070). Every
harness clud launches from a console is a TUI that expects a terminal; where
one ran as a subprocess, that was incidental rather than designed. Owning the
byte stream is what gives clud its PTY features (toasts DD-071, Ctrl+V image
paste, drag-drop path normalization, F3 voice).

**Decision:** One rule for every harness, command and platform. `--pty` /
`--subprocess` win. Otherwise PTY if and only if clud has a real terminal
(stdin and stdout both TTYs) and the launch is not headless (Claude
`-p`/`--print` from clud or passthrough, `codex exec`, DeepSeek's headless
profile). Everything else is subprocess: clud never fabricates a PTY when it
has no terminal. This retires the per-harness, per-platform, `clud loop` and
`grind` special cases and the `CLUD_PTY_DEFAULT` audit lever.

**Rationale:** The TTY gate avoids the one hard hazard: redirected stdout
(ConPTY hangs, and `clud -p ... > out.txt` must stay byte-exact). Headless
launches keep subprocess so stream-json and final output reach stdout
unmodified. ConPTY's attach repaint (#515) and the pump's idle polling are
real costs, but a harness TUI running without clud in its byte path loses
more than they cost; the pump budget is its own work item.

**Consequences:** Interactive sessions pay the pump's idle polling (#691's
cost table) on every platform. Claude `clud loop` runs `claude -p`, so it is
subprocess and streams progress through stream-json. Codex and `grind`
without a terminal are subprocess. Guard: `backend::launch_mode_tests`.

## DD-087: `grind` is a skill DAG with capped agent roles

**Status:** Accepted. Supersedes DD-068's prompt text and retires
`/clud-meta-work`.

**Context:** Two orchestrators had grown for the same job. `/clud-meta-work`
was a portable prose playbook with nothing enforcing it. A user-local
`meta-work` workflow was deterministic but required `act` under `bosn` for
every goal, built in a fresh worktree per goal, and admin-merged without
waiting for CI. That burned CPU, cold-started caches, and relied on prompts
alone to keep workers from building.

**Decision:** `clud grind` seeds `/grind`, a router skill over
`grind-intake → plan → work → review → integrate → land` leaf skills. A
bundled `grind` workflow runs the parallel and sequential modes; cron mode is
the harness's `/loop` over sequential runs of one issue each. Every workflow
agent runs as a bundled `grind-<role>` agent type whose `tools:` list is its
hard tool cap. Shell commands are capped per role by clud's existing
PreToolUse hook, which reads the payload's `agent_type`. One integrator
builds at a time, at most four other agents run at once, and `act` is
optional, offered only when Docker works and `ci.yml` exists.

**Rationale:** Skills keep each step readable and invocable on its own. The
workflow makes ordering, concurrency and fix-round limits deterministic.
Agent types plus the hook turn "workers do not build" from a request into a
rule. Reusing the native hook adds no per-call process: it already runs on
every shell call. Frontmatter-scoped hooks were tried and did not fire for
subagents.

**Consequences:** `grind` stays Claude-only; Codex keeps no inline fallback,
since the Workflow tool is the point. A new role needs its agent file, a
`claude_files.rs` entry and a policy in `block_bad_cmd_grind_caps.rs`. The
contract is owned by [architecture/grind.md](architecture/grind.md).

## DD-088: The PTY pump blocks on one event channel, and CI must prove it under a real console

**Status:** Accepted

**Context:** #1310, following DD-086. Every console launch now runs through
the PTY pump, but its main loop polled stdin every 5 ms: about 200 idle
wakeups a second per session. Nothing measured that cost, since
`bench/idle_cpu` covered only daemon subprocess sessions. Nothing proved the
pump works on Windows either. CI ran every Rust harness with stdout piped, and
`require_pty_or_skip!` quietly skipped each PTY test when its canary failed.
Running the `pty` harness inside a pseudo-terminal with the skip turned into a
failure showed that 18 of its 22 Windows tests had never actually run.

**Decision:** stdin, `extra_rx`, resize, and the output reader's close notice
all feed one `PumpEvent` channel. The main loop blocks on it until the next
event or a 50 ms tick, which re-checks the interrupt flag, the hooks, and
Windows child exit. `bench/idle_cpu --mode pty` measures foreground `clud
--pty` sessions against committed N=1 and N=6 baselines, using a throwaway
HOME and working directory. `ci/run_bundle.py` runs the `pty` harness inside
a pseudo-terminal with `CLUD_REQUIRE_PTY=1`, and the canary answers ConPTY's
`ESC[6n` cursor query the way a terminal would. The `ci-windows` PR label
runs only static checks plus Windows x64, so Windows fixes iterate quickly.
`CI OK` passes exactly when those lanes pass. That is
safe because the merge queue always runs the full matrix. An always-red
gate was tried first, but `pr_merge_watch` cancels a run on the first red
`CI OK`, which made the label unusable.

**Rationale:** An event wakes the loop immediately, so keystroke latency
stays where DD-018 put it. Only the idle re-checks move to the tick.
Measured on Linux with the same host and 60 s windows, run back to back, one
idle session went from 11,314 to 1,221 context switches and from 1.89 to
0.92 CPU-seconds. Six sessions went from 69,692 to 7,335 and from 7.6 to 5.08.
A skip that reads as green is worse than no test, so the gate fails loudly
and prints the bytes the canary received.

**Consequences:** The Windows resize watcher (150 ms) and the
`console_input` adapter (100 ms slices) still poll. Making them event-driven
needs upstream `running-process` support. A dev build of clud must never see
the real HOME: it rewrites `~/.clud/state/rm-shim/rm`, and the installed
`clud-cmd-scan` hook then denies every shell command.

## DD-089: Claude commit/PR attribution is opt-in under clud

**Status:** Accepted

**Context:** #1317. Claude Code adds a `Co-Authored-By: Claude …` trailer to
every commit and a "Generated with Claude Code" line to every PR body. It
exposes both as `attribution.commit` and `attribution.pr` in its settings
(`includeCoAuthoredBy` is the deprecated spelling), and an empty string hides
each one. No environment variable controls them.

**Decision:** clud hides both by default. `--coauthor` (or `CLUD_COAUTHOR=1`)
keeps Claude Code's own attribution, and `--coauthor=TAG` (or
`CLUD_COAUTHOR=TAG`) uses TAG for both. The choice is
`LaunchPlan::coauthor`, shown in `--dry-run`. The runtime merges it into the
launch's single `--settings` document, the same one hooks and the status line
use, and adds only the keys the user's own `--settings` left unset. The TAG is
`=`-joined only, so a bare `--coauthor` never swallows the next word.

**Rationale:** The trailer is a statement about authorship that the user, not
the harness, should choose to make. Carrying the choice on the plan rather
than on the plan's argv keeps every existing argv contract intact, including
a prompt that stays last. Settings the user wrote themselves outrank clud's
default. Codex and the DeepSeek harness add no such lines, so the flag does
nothing on them.

**Consequences:** Like the status line (DD-071), this makes every Claude
launch carry a launch-scoped `--settings`, even in a repo that declares no
hooks. `--coauthor` restores the old argv.

## DD-090: Test Claude Code integrations by mocking the model, not the harness

**Status:** Accepted.

**Context:** Everything clud installs into Claude Code (skills, agent types,
workflows, hooks) is interpreted by Claude Code itself. `mock-agent` replaces
the harness, so it can prove how clud *launches* Claude Code but nothing about
what Claude Code then *does*. `/grind` shipped with its workflow never
executed, and several of its assumptions were answerable only by running
it: do workflow agents' tool calls reach hooks, and how are skills rendered?

**Decision:** Add a third test tier that runs the real, pinned Claude Code
with `ANTHROPIC_BASE_URL` pointed at `mock-agent serve`, which plays the
model from a per-role script (see `docs/architecture/testing-tiers.md`). The
`tests/harness/` fixture installs clud's real assets into an isolated config
with `clud install-assets --home`, and asserts on the requests that reached
the model, the hook log and the world state.

**Rationale:** Faking the model keeps the part under test real. A step keyed
on the assistant-turn count makes each request self-describing, so the server
needs no state, and Claude Code's extra user messages cannot desynchronize it.
It is opt-in because it needs an installed Claude Code; it runs in CI's full
mode against a pinned version, because the request shapes it answers are
version-specific.

**Consequences:** Upgrading the pinned Claude Code can break the tier and
should be done deliberately. The fixture has to work around clud's own
session machinery (the `python` shim, the rm-identity check), as its module
docs describe.

## DD-091: `/grind` gets repo lint/test scripts from a clud subcommand, asked once per run

**Status:** Accepted.

**Context:** zackees/clud#1336. Many repos ship `./lint` and `./test` entry
scripts, and `/grind`'s integrator should run them before every push. Finding
them depends on the platform (`.bat`/`.ps1` on Windows, extensionless or
`.sh` elsewhere), on whether the file is executable (`bash ./lint` if not),
and on what the script hands off to (`python -m ci.test` means `ci/test.py`).
A goal list can hold many goals from the same repository.

**Decision:** `clud grind-scripts` detects the scripts and prints each one's
kind, path, run command and the files to read for modes. The `/grind` router
gets that output through a `` !`clud grind-scripts` `` line, the way `/do` gets
`clud do-prompt` (#1322). The model's only job is to read the listed files
and pick out the user-facing modes. It never runs the scripts. The router asks
one question per run, records the answer as `scripts` in
`.clud/grind/run.json`, and passes it to the workflow as `args.verify`.

**Rationale:**

- **The subcommand, not the model, does detection.** The steps are fixed
  rules: which candidate wins, whether the file is executable, which
  interpreter to use. A model probing the filesystem could get any of them
  wrong, and a scripted-model harness can't test what it does. A subcommand is
  unit-tested, and the harness can check its rendered output in the router's
  first request. What's left for the model is reading, where it beats a
  regex, which can't follow delegation and picks up tool flags.
- **One question per run.** The scripts belong to the repository, not to a
  goal, so every goal gets the same answer. Asking once per goal would repeat
  the question N times. A workflow can't ask questions while it runs, so the
  router asks everything before it starts.

**Consequences:** If no scripts are detected, there's no question and the
planner's verify commands stand. When scripts are chosen, the planner only
supplies the goal's focused test. Adding a candidate or interpreter means
changing `grind_scripts.rs` and its tests, not the router text. The contract is
owned by [architecture/grind.md](architecture/grind.md#repository-linttest-scripts).

## DD-092: clud indexes Claude-harness sessions per cwd and recovers them portably

**Status:** Accepted

**Context:** #922. `clud -c` forwarded `--continue` and left the rest to
Claude Code, which cannot say which provider created a session, list a
directory's sessions, pick a compact checkpoint, or move a conversation onto
a model with a smaller window. A Codex-via-Claude session also keeps its
canonical history in the bridge process, so it cannot be resumed natively
once that process is gone. Claude's transcripts do hold everything needed,
but a local survey found thousands of them, so scanning on every launch is
the wrong steady state.

**Decision:** clud keeps a small per-cwd index under its state directory,
fed by a `clud session-hook` that every Claude-harness launch registers for
`SessionStart`, `PostCompact` and `SessionEnd` with the route resolved in the
`LaunchPlan`. Existing transcripts are imported once per cwd, with the route
inferred from the model name and flagged as such. At a terminal, `clud -c`
opens a picker, and `--last` takes the newest session. `--resume-mode auto`
resumes natively when that is safe. It uses a forked native resume for a
compatible provider switch, and portable recovery otherwise: a new session
seeded through `SessionStart.additionalContext` with a marked, truncated
context (compact summary plus the newest whole turns within half the
destination window). Without a terminal, `-c` keeps Claude's `--continue`.

**Rationale:**
- The transcript stays the only source of conversation content, so the index
  can't drift from it. It holds pointers and metadata, and a corrupt index is
  simply rebuilt.
- Recording the route at launch is authoritative. Model names are ambiguous
  across gateways, so they are used only for legacy import, and an inferred
  route never overwrites a recorded one.
- The size estimate uses the content a resume would replay, never cumulative
  usage, which overstates the live context by orders of magnitude.
- Following `parentUuid` rather than file order is what makes a rewound
  session recover the branch the user was actually on.

**Consequences:**
- Every Claude-harness launch now carries clud's session hook in its
  launch-scoped `--settings`, in the same way the status line (DD-071) and
  attribution (DD-089) are carried.
- Previews, summaries and recovery files are local sensitive data. They are
  locked, written atomically, owner-only, and kept out of logs and `--dry-run`.
- Native Codex-harness history is out of scope.


## DD-093: Anthropic-compatible providers are descriptor rows, and the gateway routes them as one list

**Status:** Accepted

**Context:** #936 and #937. DeepSeek arrived first and was wired by hand:
its own vault identifiers, overlay function, preflight, settings strings, and
two gateway fields (`deepseek_api_key`, `deepseek_base_url`) with matching
`match` arms. OpenRouter copied the pattern. Adding Kimi that way would have
meant a third copy at every site, and the silent fallbacks (`_ =>` arms,
hand-written provider arrays) meant a missed site would route a request to the
wrong provider rather than fail to compile.

**Decision:** An Anthropic-compatible provider is one `&'static`
`AnthropicCompatProvider` row in `provider_registry::ANTHROPIC_COMPAT_PROVIDERS`
plus its `provider_catalog::MODELS` rows. Shared code (vault access, preflight,
child-env overlay, settings, TUI, auth) is parameterized by the row. The
unified gateway holds one `AnthropicCompatRoute { provider, base_url, api_key }`
per provider whose key the launch holds, and a single `provider_available`
predicate drives discovery, the refusal ID list, and dispatch. Kimi is routed
directly to Moonshot's Anthropic-compatible endpoint, never through a
translation bridge.

**Rationale:**
- A data table beats a `dyn Provider` trait: every provider is known at
  compile time, the rows are plain data a test can iterate, and it matches the
  repository's existing registries (`CatalogModel`, `BUNDLED_SKILLS`,
  `BUNDLED_TOOLS`). A trait would add dynamic dispatch and hide the
  differences, which are all data, behind methods.
- Guardrail tests iterating `ModelProvider::ALL` turn a forgotten site into a
  test failure instead of a silent fallback.
- Direct routing keeps Kimi's credential on one hop: clud's vault to the
  gateway (or child env) to Moonshot. A translation bridge would add a
  second process holding the key and a second protocol to keep faithful, for
  an endpoint that already speaks Anthropic Messages.
- Vault identifiers are part of the stored credential's address, so each
  row's `vault_service`/`vault_account` is frozen by a test. Renaming one
  orphans every key already stored under it.

**Consequences:**
- A new Anthropic-compatible provider is a descriptor row, a catalog row, an
  enum variant, and its clap flag; CLAUDE.md's "New model provider" entry lists
  the steps and the test that catches each omission.
- Provider-side gaps are documented, not repaired: Kimi's endpoint does not
  support Claude Code's WebFetch, and clud does not emulate it.
- Credentials still never cross the daemon wire, `LaunchPlan`, or dry-run
  output; the route list lives only inside the launch-scoped gateway.

---

## DD-094: `gh pr merge --auto` is allowed; only local watchers are denied

**Status:** Accepted (amends DD-065)

**Context:** DD-065 put `gh pr merge --auto` on the `git.pr_wait_fail_fast`
deny list next to `gh pr checks --watch`. But `--auto` does not wait locally:
it enqueues the merge with GitHub and returns at once, so it never ties up an
agent the way a local watcher does, and it is the normal way to say "merge
when green".

**Decision:** The guard denies only local waiters (`gh pr checks --watch`,
`gh run watch`, hand-rolled polling loops), which `pr_merge_watch.py`
replaces because it fails fast. `gh pr merge --auto` is allowed.

**Consequences:** A red lane on an auto-merge PR no longer cancels the rest
of the matrix unless someone also runs `pr_merge_watch.py`; the merge simply
never happens. That is an acceptable cost for keeping GitHub's native
auto-merge available.

## DD-095: The /grind feature lands as a merge commit, and main is merged into the feature branch rather than rebased

**Status:** Accepted

**Context:** #1410, spec #1392 §6. In feature-branch mode each goal PR merges
into `grind/meta-<M>-<run-id>`, and only one feature PR reaches `<main>`.
There are three ways that PR could land, and two ways to bring in a `<main>`
that moved during the run.

**Decision:** The feature PR always lands as a merge commit (`--no-ff`,
`gh pr merge --merge`), whoever merges it. When `<main>` moves, grind merges
`<main>` into the feature branch and never rebases it.

**Rationale:**
- The goal commits that land are the ones that were tested, SHA for SHA, so
  nothing needs re-testing.
- History keeps one commit per goal.
- `git revert -m1 <merge>` undoes the whole feature in one step.
- A rebase replays N goal commits onto the moved `<main>`, so the code that
  lands is not the code that was tested. On a long refactor it also forces
  conflict resolution one commit at a time, and undoing the feature means
  reverting N commits. A squash is easy to revert but loses per-goal history.

**Consequences:** `<main>` gets one merge commit per feature and the feature
branch may carry merge commits from `<main>`. The hook refuses `--admin` on
the feature PR, and the lander merges it only under `feature_merge: auto`.
The contract is owned by
[architecture/grind.md](architecture/grind.md#feature-branch-mode).

## DD-096: The feature PR is the single closer of /grind issues, and `clud grind reconcile` reopens early closes

**Status:** Accepted

**Context:** #1393. In feature-branch mode a child's fix merges into
`grind/meta-<M>-<run-id>`, not `<main>`. GitHub closes an issue from a
closing keyword only when the PR merges into the default branch, but people
and tools still close issues early: by hand mid-run, from a PR merged into a
non-default base, or from a commit that never reaches `<main>`. If the
feature PR is then closed unmerged, those issues read as done while their fix
exists only at `refs/pull/<N>/head`. Nothing on GitHub records which feature
PR an issue is waiting on.

**Decision:**
- **One closer.** Goal PRs say `Refs #N`; only the feature PR carries
  `Closes #N` (and `Closes #<meta>`), so GitHub closes each issue exactly when
  its fix reaches `<main>`. No grind role closes an issue by hand.
- **Label plus marker.** The lander labels every issue whose goal merged into
  the feature branch `grind:on-feature` and posts
  `<!-- grind:v1 feature-pr=#N branch=... goal-pr=#G run=... -->`. The label
  makes the set queryable; the marker says which PR it waits on.
- **Reconcile.** `clud grind reconcile` (run by the `/grind` router and each
  `/grind-cron` tick) walks every labelled issue. An issue closed by anything
  other than a merge into `<main>` (or a commit reachable from `<main>`) is
  reopened while its feature PR is unlanded. A feature PR closed unmerged
  reopens and unlabels its issues and cites `refs/pull/N/head`. A feature PR
  merged into `<main>` closes any stragglers and removes the label.

**Rationale:** GitHub's own closing semantics already mean "the fix is on the
default branch"; making the feature PR the only closer reuses them instead of
simulating them. Reconcile is idempotent and reads only GitHub state, so it
can run on every tick and repair damage done outside grind.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Projects v2 status field | Needs a project per repo and extra token scopes; a label and a comment work with the default `gh` login and are visible on the issue itself. |
| Milestones | One milestone per issue, often already used for releases; it cannot name the PR an issue waits on. |
| Rulesets / branch protection | They gate merges, not issue state; nothing stops a hand close. |
| Close on goal merge, reopen on failure | Issues read as fixed while the fix is off `<main>`, the exact state #1393 forbids. |

**Consequences:** An issue can be closed early, but only until the next
reconcile. The label is removed only when the feature PR has merged or been
closed, so an open label always means "fix pending on a feature branch".
Intake skips `grind:on-feature` issues. The contract is owned by
[architecture/grind.md](architecture/grind.md#never-losing-issues-in-feature-mode).

## DD-097: The /grind run plans before it asks: one up-front question round, none after prework

**Status:** Accepted

**Context:** #1407, spec #1392 §0 and §2. A workflow cannot ask questions
while it runs, and `/grind` runs unattended for hours. Earlier drafts asked
questions as they came up: repo state at the start, feature grouping after
planning, the merge policy when the feature PR opened. Each one stalled a run
the user had walked away from, and a question about grouping cannot be asked
well before the children have been classified.

**Decision:** The router routes the input, runs a plan-only classification
pass (no edits, worktrees or pushes), inspects repo state, and only then asks
**one** question round of at most two `AskUserQuestion` calls. Every answer
(dirty-repo action, regroup, mode, models, CI, scripts, feature merge policy,
problem reporting) is recorded in `run.json` and the plan before prework
starts. From prework on nobody asks: the hook denies `AskUserQuestion` to
every `grind-*` subagent, and anything unexpected follows a rule recorded in
advance (`stuck_bug`, `no_overlap`, `problem_reporting`).

**Rationale:**
- Planning first means the questions are about a concrete plan, so the user
  answers once with the facts in front of them.
- One round bounds interruption: after it the user can leave.
- Recorded rules make every mid-run decision reproducible from the plan
  comment, which nobody edits.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Ask as questions arise | Stalls an unattended run at an unknown time. |
| Ask before planning | Grouping and merge-policy questions would be guesses. |
| No questions, fixed defaults | Dirty-repo handling and merge policy are the user's call. |

**Consequences:** The plan-only pass costs one planner agent before any
question. Intake's conversion (or pick) question stays separate and precedes
the round, because it decides what the meta issue is. The contract is owned by
[architecture/grind.md](architecture/grind.md#preflight-and-the-single-question-round).

## DD-098: /grind follow-up issues are never sub-issues of the meta

**Status:** Accepted

**Context:** #1411, spec #1392. Roles find problems outside their goal's
scope. Filed as sub-issues of the meta issue, they would widen the run's
scope while it runs: the meta could not close until they were fixed, and the
no-overlap check and reconcile would count them.

**Decision:** Roles return problems; only the router files them. Under
`problem_reporting: issue` each becomes an issue labelled `grind:followup`,
with `Refs #<meta>` and a `<!-- grind:followup ... -->` marker naming its
stage and feature PR, and it is never attached as a sub-issue of the meta.
Intake and cron pick a follow-up up later: at once for the bug stage, and
after the feature PR merges for a feature stage.

**Rationale:** The meta's sub-issue list stays exactly the run's scope, so
closing the meta, the no-overlap check and reconcile act on precisely the
issues the run planned. The label and marker keep follow-ups queryable and
linked without that coupling.

**Consequences:** A follow-up is visible from the meta only through its
`Refs` backlink. A failed filing is listed in the router's final report
rather than stopping the run. The contract is owned by
[architecture/grind.md](architecture/grind.md#problem-reporting).

## DD-099: The /grind router starts grind-run once per stage

**Status:** Accepted

**Context:** #1409, spec #1392 §3. The feature branch must be cut from
`<main>` after the bug stage has merged, so it contains those fixes. Only
the router (the main session) may create the feature worktree, and a
Workflow call is one tool call: the router cannot act between two stages of
the same call. Cutting the branch before the call breaks that ordering.

**Decision:** The router starts `grind-run` twice. The bug-stage call gets
the plan and the bug children; its prework posts the plan and the result
returns `plan_url`. After the router cuts the feature branch and opens the
draft feature PR, the feature-stage call gets the feature children, the
feature branch as `base`, `plan_url` (so prework is not repeated), `feature`,
`feature_merge` and `stuck_bugs`, the bug children that did not merge.

**Rationale:** Each call keeps the workflow deterministic and needs no role
with the router's worktree rights. The stuck-bug rule stays in the workflow,
not the router's prose, because `stuck_bugs` carries the only fact it needs
across calls.

**Consequences:** An all-feature plan still makes a bug-stage call with no
goals, so the plan is recorded before any branch exists. `grind-run` still
orders stages when one call is given goals from both. The contract is owned
by [architecture/grind.md](architecture/grind.md#bug-stage-then-feature-stage).
