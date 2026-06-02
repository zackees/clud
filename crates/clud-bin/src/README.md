# src/

Entry point and source tree for the `clud-bin` Rust binary. The binary launches a backend agent (`claude` or `codex`) in YOLO mode, optionally through a PTY, with first-class support for loop iterations, drag-and-drop, voice input, and a per-user daemon for backgrounded/detachable sessions. `main.rs` does cross-cutting startup work (trampoline unlock, console title, skill install, session-cap registration, GC scanner) and then hands off to `runner.rs`, which drives the per-iteration subprocess/PTY launch loop for a single [`LaunchPlan`]. Submodules under `command/`, `daemon/`, `dnd/`, and `voice/` carry the bulk of the domain logic; the top-level `.rs` files here are the orchestration glue and standalone utilities consumed by both `main.rs` and the integration tests.

## Subdirectories

- [command/](command/README.md) — `LaunchPlan` construction: backend argv assembly, YOLO/safe injection, `loop`/`up`/`rebase`/`fix` prompt synthesis, `--repeat` schedule parsing, DONE/BLOCKED contract.
- [daemon/](daemon/README.md) — long-lived session manager for `--detach` / `attach` / `list` / `kill` / `logs` / `--repeat`: TCP JSON IPC, per-session worker subprocesses, snapshot + log persistence, attach broker.
- [dnd/](dnd/README.md) — drag-and-drop into the terminal: cross-platform path-string normalizer plus Windows-only `IDropTarget` adapter with per-launch-mode injectors (subprocess `WriteConsoleInputW`, PTY master writer).
- [voice/](voice/README.md) — F3 push-to-talk voice mode: `cpal`/`arecord` mic capture, start/stop cues, `whisper-rs` worker thread, transcript injection into the backend PTY.

## Top-level modules

Entry and orchestration:

- `main.rs` — process entry: launch clock, trampoline unlock, console title stamp + keeper, skill install, large-file guard, session-cap registration, GC scanner, dispatch to runner / daemon / hook-health / GC subcommands.
- `lib.rs` — library facade so integration tests under `tests/` can link against internals; `main.rs` imports through this rather than re-declaring `mod ...`.
- `runner.rs` — per-iteration subprocess- and PTY-mode runner for a single `LaunchPlan`; owns child-env construction, stream-json fallback, Ctrl-C-aware teardown, and OLE drag-drop registration wiring.
- `startup.rs` — launch-time helpers factored out of `main.rs`: drag-target gating (`--no-dnd`, `--dry-run`), session-cap enforcement, Ctrl+C flag installer.

CLI surface and backend resolution:

- `args.rs` — `clap` `Args` and `Command` definitions; passthrough for unknown flags; subcommand definitions for `loop`, `up`, `rebase`, `fix`, `gc`, etc.
- `backend.rs` — `Backend` enum (`Claude` / `Codex`), `LaunchMode` (`Subprocess` / `Pty`), PATH lookup, and backend-path resolution.
- `subprocess.rs` — single decision point for the Windows `.cmd`/`.bat` rewrite (BatBadBat / CVE-2024-24576) via `running-process-core`'s `CommandSpec::Shell`.

Console and terminal:

- `console_input.rs` — Windows `ReadConsoleInputW` translator (issue #141): pure-function map from `KEY_EVENT_RECORD` slices to PTY stdin bytes (Shift+Enter → `\n`, plain Enter → `\r`).
- `console_setup.rs` — RAII guard that enables `ENABLE_VIRTUAL_TERMINAL_INPUT` for the lifetime of a PTY session and restores the prior console mode on drop; no-op on POSIX.
- `console_title.rs` — stamps `clud <cwd-name>` once on launch and runs a background keeper that re-applies the title when downstream OSC 0/2 sequences overwrite it.
- `capture.rs` — server-side terminal emulator (`vt100` + `vte` sticky-mode sniffer) that lets the daemon synthesize a repaint when a mid-session client attaches.
- `session.rs` — raw-PTY pump (`run_raw_pty_pump`), resize handling, F3 voice observer hook, OSC-title stripper integration, dropped-path injection on the PTY master.

Loop subsystem (`clud loop`):

- `loop_spec.rs` — task-spec resolver: classifies the positional (GH URL, `#42`, file, literal), fetches GH issue/PR bodies via `gh` (curl fallback), caches under `.clud/loop/`, locates DONE/BLOCKED marker files.
- `loop_check.rs` — post-iteration DONE/BLOCKED marker check; file-only and stdout-scanning variants used by PTY and subprocess paths respectively.
- `loop_artifacts.rs` — durable `<git-root>/.clud/loop/` artifacts: `info.json` (`TaskInfo`), `log.txt`, `motivation.md`, and `.gitignore` auto-injection.
- `stream_json.rs` — pure renderer for claude's `--output-format stream-json` events; turns one JSON event per line into one human-readable progress line for subprocess-mode loops.

Process management and GC:

- `process_tree.rs` — best-effort descendant-tree termination via `sysinfo`; fixes the multi-second Ctrl+C hang for `clud --codex` on Windows where `cmd.exe → node.exe` would orphan the real child.
- `session_registry.rs` — `redb`-backed registry of live `clud` PIDs that caps concurrent siblings (default 64, `CLUD_MAX_INSTANCES`); `Drop` removes the row, startup GCs dead rows.
- `gc.rs` — `clud gc list` / `purge` / `reconcile` CLI handlers (thin IPC clients against the always-on daemon) and the in-process `WorktreeScanner` thread that polls `.claude/worktrees/agent-*` for new entries. The GC registry itself lives inside the daemon (see `daemon/gc_service.rs`).
- `worktrees.rs` — `--clean-worktrees` (issue #83): enumerates via `git worktree list --porcelain`, classifies clean / dirty / unpushed / gone, removes safe ones; `--dry-run` faithful.

Platform glue:

- `trampoline.rs` — Windows-only: rename-self-and-copy-back trick so `pip install` can always overwrite `Scripts/clud.exe`. No-op on POSIX.
- `win_creation_flags.rs` — `invisible_helper_creationflags()` returns `CREATE_NO_WINDOW` on Windows for daemon-helper spawns; `0` elsewhere so call sites stay portable.
- `large_file_guard.rs` — startup-time `ignore`-crate walker that warns about source files large enough to choke agents (issue #132); hard 1 s deadline.

Skills and hooks:

- `skills.rs` — bundles slash-command skills via `include_str!` and installs them per-backend (`.claude/skills/`, `.codex/skills/`) only when the backend home already exists; never overwrites existing files.
- `skill_install.rs` — auto-installer for the `clud-*` skill set; compares embedded vs installed `SKILL.md` modulo whitespace and overwrites divergent copies (logging `[clud] updated /<name>`).
- `hook_health.rs` — `PreToolUse` hook parity diagnostics and `--fix-hooks` remediation (deterministic config edits plus optional agent-driven semantic hook translation).
- `codex_hook_normalize.rs` — issue #234: idempotent global pass that bumps any `~/.codex/hooks.json` handler `timeout: 5` to `30` (`~/.clud/settings.lock` fs4 guard, green status line on change).

Diagnostics and misc:

- `verbose_log.rs` — launch-clock + opt-in file logging (`CLUD_VERBOSE_LOG_DIR`); `log()` writes timestamped lines to the per-launch log file.
- `wasm.rs` — `wasmi`-based runner that loads a WASM module, registers a minimal `host.log` import, invokes a named export, and propagates the integer exit code.

Quick lookup — which file owns a given subcommand:

- `clud loop ...` → `command::build_launch_plan` (prompt + markers) + `loop_spec` (task resolution) + `loop_artifacts` (artifact files) + `runner.rs` (iteration loop) + `loop_check` (DONE/BLOCKED scan).
- `clud --detach`, `clud attach`, `clud list`, `clud kill`, `clud logs` → all in `daemon/` (dispatched from `daemon::handle_special_command`).
- `clud gc list` / `purge` / `reconcile` → `gc.rs` (CLI handlers) talking to `daemon/gc_service.rs` (registry owner inside the always-on `__daemon`).
- `clud --clean-worktrees` → `worktrees.rs`.
- `clud --fix-hooks` → `hook_health.rs`.

## Cross-cutting subsystems

Subsystems that span multiple files have their own topic docs under `docs/architecture/`:

- **Loop subsystem** (`command/`, `loop_spec`, `loop_check`, `loop_artifacts`, `stream_json`, `runner`) → [docs/architecture/loop-subsystem.md](../../../docs/architecture/loop-subsystem.md)
- **Daemon IPC** (everything under `daemon/`) → [docs/architecture/daemon-ipc.md](../../../docs/architecture/daemon-ipc.md)
- **Session lifecycle** (`session`, `console_*`, `capture`, `dnd` injection, `voice` hooks) → [docs/architecture/session-lifecycle.md](../../../docs/architecture/session-lifecycle.md)
- **Skill system** (`skills`, `skill_install`, `assets/skills/`) → [docs/architecture/skill-system.md](../../../docs/architecture/skill-system.md)
- **GC and registry** (`gc`, `daemon/gc_service`, `session_registry`, `worktrees`) → [docs/architecture/gc-and-registry.md](../../../docs/architecture/gc-and-registry.md)
- **Windows quirks** (`trampoline`, `subprocess` BatBadBat, `console_*`, `dnd`, `win_creation_flags`, `voice` ARM carveout) → [docs/architecture/windows-quirks.md](../../../docs/architecture/windows-quirks.md)
- **Launch plan** (`command/types::LaunchPlan` + all consumers) → [docs/architecture/launch-plan.md](../../../docs/architecture/launch-plan.md)

Non-obvious design choices (single `LaunchPlan`, `lib.rs` as the only `mod ...` site, cooperative Ctrl+C, redb single-owner) have ADRs in [docs/DESIGN_DECISIONS.md](../../../docs/DESIGN_DECISIONS.md).

## Entry point

`main.rs` is the binary entry; `lib.rs` re-exports every top-level module (and the four subdirs) as `pub mod ...` so integration tests under `crates/clud-bin/tests/` can link against internals (notably `session::run_raw_pty_pump` and `session::F3Observer`). See [DD-007](../../../docs/DESIGN_DECISIONS.md#dd-007-librs-is-the-only-place-that-declares-modules-mainrs-imports-through-clud) for why the single-instantiation pattern matters.

## See also

- Parent crate overview: [`../README.md`](../README.md).
- Top-level project docs and CI matrix: [`../../../CLAUDE.md`](../../../CLAUDE.md).
