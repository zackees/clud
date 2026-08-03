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
- The skill system needs to handle two install targets, which complicates [DD-008](#dd-008-dual-skill-installer-skillsrs-vs-skill_installrs-interim-state).

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

**Context:** Clud bundles `SKILL.md` playbooks for `/clud-pr`, `/clud-issue`, etc. inside the binary and writes them to per-backend user directories during global setup. PR #243 (closing issue #241) moved the Codex install target from `~/.codex/skills/` to `~/.agents/skills/`, on the belief that Codex had adopted a shared cross-vendor `~/.agents/` convention. In practice, Codex CLI loads skills from `~/.codex/skills/` and never consulted `~/.agents/skills/`. The visible symptom: `clud --codex -p "/clud-pr <issue>"` did not resolve `/clud-pr` even though the SKILL.md was installed — Codex never looked at the file. Reported in #289; meta burn-down at #299.

**Decision:** Codex skills install to `~/.codex/skills/<name>/SKILL.md`, the same layout Claude uses at `~/.claude/skills/<name>/SKILL.md`. Existing clud-managed copies under `~/.agents/skills/` are purged best-effort on first Codex global setup after upgrade (`purge_stale_agents_skills` in `skills.rs`). The purge applies the same conservative rules as the prior `~/.codex/skills/` purge: only delete a `SKILL.md` that contains the `managed-by: clud` marker and lives under a currently bundled skill name; leave unrelated files and user-authored skills alone.

**Rationale:**

- The whole point of installing the file is for the backend to find and execute it. Installing somewhere the backend ignores is worse than not installing at all — it consumes disk, suggests false coverage in tests, and masks the real bug.
- Mirroring Claude's layout eliminates a backend-specific branch in `SKILL_BACKENDS`: both entries now use `skills_home_subdir: None` (the field stays for future backends whose skills live outside their config root).
- Skip-if-exists still preserves user-edited skills at the new location.
- The cleanup of `~/.agents/skills/` is symmetric to the prior `~/.codex/skills/` cleanup pattern, so users upgrading don't end up with stale duplicates.

**Alternatives Considered:**

| Approach | Why not |
|---|---|
| Keep installing to `~/.agents/skills/` and add runtime slash-command expansion inside `push_prompt` (intercept `/clud-pr ...` and inline the SKILL.md body before passing to `codex exec`) | Doubles the surface area (install path + runtime translation), tightly couples `command/prompts.rs` to skill discovery, and gives nothing for interactive Codex users. The install-to-the-right-place approach is strictly simpler. |
| Install to both `~/.codex/skills/` and `~/.agents/skills/` | Two copies on disk drift apart over time when users edit one. No real consumer of `~/.agents/skills/` has been identified. Add a second target only when a real need surfaces. |
| Install to `~/.codex/prompts/<name>.md` (Codex's documented custom-prompts location) | Requires a different format (plain markdown, no YAML frontmatter, no trigger metadata) and loses skill semantics. Worth revisiting separately if Codex's skill loader ever changes. |

**Consequences:**

- `clud --codex -p "/clud-pr 123"` works end-to-end on first global setup after upgrade.
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
      "install_soldr": true,
      "soldr_version": "0.7.11"
    }
  }
}
```

The parser accepts both forms. Direct `rust` keys win over `optimize.rust`
keys inside the same file; repo-level values still win over user-level values
per field.

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
`401` no credentials, upstream `4xx` passed through, `502`/`504` otherwise);
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
