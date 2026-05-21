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
- `gc.rs` — `clud gc list` / `purge` / `reconcile` and the in-process `WorktreeScanner` thread that polls `.claude/worktrees/agent-*` for new entries.
- `gc_daemon.rs` — single-owner GC daemon process: holds `~/.clud/data.redb` exclusively and serves JSON-over-loopback-TCP from a registry-worker thread (issue #135 Phase 1).
- `worktrees.rs` — `--clean-worktrees` (issue #83): enumerates via `git worktree list --porcelain`, classifies clean / dirty / unpushed / gone, removes safe ones; `--dry-run` faithful.

Platform glue:

- `trampoline.rs` — Windows-only: rename-self-and-copy-back trick so `pip install` can always overwrite `Scripts/clud.exe`. No-op on POSIX.
- `win_creation_flags.rs` — `invisible_helper_creationflags()` returns `CREATE_NO_WINDOW` on Windows for daemon-helper spawns; `0` elsewhere so call sites stay portable.
- `large_file_guard.rs` — startup-time `ignore`-crate walker that warns about source files large enough to choke agents (issue #132); hard 1 s deadline.

Skills and hooks:

- `skills.rs` — bundles slash-command skills via `include_str!` and installs them per-backend (`.claude/skills/`, `.codex/skills/`) only when the backend home already exists; never overwrites existing files.
- `skill_install.rs` — auto-installer for the `clud-*` skill set; compares embedded vs installed `SKILL.md` modulo whitespace and overwrites divergent copies (logging `[clud] updated /<name>`).
- `hook_health.rs` — `PreToolUse` hook parity diagnostics and `--fix-hooks` remediation (deterministic config edits plus optional agent-driven semantic hook translation).

Diagnostics and misc:

- `verbose_log.rs` — launch-clock + opt-in file logging (`CLUD_VERBOSE_LOG_DIR`); `log()` writes timestamped lines to the per-launch log file.
- `wasm.rs` — `wasmi`-based runner that loads a WASM module, registers a minimal `host.log` import, invokes a named export, and propagates the integer exit code.

Quick lookup — which file owns a given subcommand:

- `clud loop ...` → `command::build_launch_plan` (prompt + markers) + `loop_spec` (task resolution) + `loop_artifacts` (artifact files) + `runner.rs` (iteration loop) + `loop_check` (DONE/BLOCKED scan).
- `clud --detach`, `clud attach`, `clud list`, `clud kill`, `clud logs` → all in `daemon/` (dispatched from `daemon::handle_special_command`).
- `clud gc list` / `purge` / `reconcile` → `gc.rs` (CLI handlers) talking to `gc_daemon` (registry owner).
- `clud --clean-worktrees` → `worktrees.rs`.
- `clud --fix-hooks` → `hook_health.rs`.

## Cross-cutting conventions

- **Single launch source of truth.** Every code path that needs to know "what would clud actually run" goes through `command::build_launch_plan` and consumes the resulting `LaunchPlan`. `--dry-run` JSON, `runner.rs`, the daemon worker, and the hook-health remediator all share this struct rather than reconstructing argv.
- **Windows-first care.** Several modules exist purely to absorb Windows quirks: `subprocess` (BatBadBat / `.cmd` rewrite), `trampoline` (exe self-rename for `pip install`), `win_creation_flags` (`CREATE_NO_WINDOW` for invisible helpers), `console_setup` (`ENABLE_VIRTUAL_TERMINAL_INPUT`), `console_input` (Shift+Enter via `ReadConsoleInputW`), and `console_title` (OSC-title keeper). All of them degrade to no-ops on POSIX.
- **Best-effort, non-fatal startup nudges.** `skills`, `skill_install`, `hook_health` (warn mode), `large_file_guard`, and `console_title` are all wrapped so a failure logs to stderr and continues — none of them can block a launch.
- **`lib.rs` is the single module instantiation site.** `main.rs` imports through `clud::{...}`; integration tests link the same library. There is no `mod ...;` declaration anywhere in `main.rs`.
- **`redb` is touched in exactly one place per file.** `~/.clud/data.redb` is owned by the `gc_daemon` process (issue #135 Phase 1); the in-process `gc::WorktreeScanner` and the `clud gc list` / `purge` clients all funnel through the daemon's JSON-over-loopback-TCP protocol. The per-launch session cap lives in a separate `redb` table accessed via `session_registry`, which serializes with a sidecar `sessions.lock` so multiple `clud` processes never race on the file.
- **Ctrl+C is cooperative.** `startup::install_ctrlc_flag` arms a `Arc<AtomicBool>` consumed by the loop iteration in `runner.rs`, the daemon attach loop in `daemon/attach.rs`, and the GC scanner thread in `gc.rs`. Forced reaping of orphan descendants is delegated to `process_tree::kill_tree`.

## Launch flow

A typical interactive launch hits the modules in roughly this order:

1. `main.rs` initializes the launch clock (`verbose_log`), unlocks the exe (`trampoline`), stamps the console title (`console_title`), installs bundled skills (`skills`, `skill_install`), checks hook health (`hook_health`), and warns on large files (`large_file_guard`).
2. `args.rs` parses the CLI; `backend.rs` resolves which agent and launch mode to use; `startup.rs` registers the session in `session_registry` (enforcing `CLUD_MAX_INSTANCES`) and installs the Ctrl+C atomic flag.
3. `command::build_launch_plan` (from `command/`) assembles the `LaunchPlan` — argv, env, prompt, optional loop markers, optional `--repeat` schedule. For `clud loop`, `loop_spec` resolves the task and `loop_artifacts` initializes `<git-root>/.clud/loop/`.
4. `main.rs` hands the plan to `runner.rs`, which dispatches to either the subprocess path (via `subprocess.rs`, with `stream_json` rendering progress) or the PTY path (via `session.rs`, with `console_input` translating Shift+Enter and `dnd` injecting drops).
5. In PTY mode `voice::VoiceMode` attaches an `InteractiveHooks` impl so F3 press/release flows the captured audio through `voice/worker.rs` and writes the transcript back into the PTY master.
6. After each iteration, `loop_check` polls DONE/BLOCKED markers; `loop_artifacts` rolls forward `info.json` / `log.txt`; on terminal signals, `process_tree::kill_tree` reaps the descendant tree before exit.

When `--detach` / `attach` / `list` / `kill` / `logs` is used, `main.rs` routes into `daemon/` instead, and the daemon process re-enters this same binary as a `__daemon` or `__worker` to host or run the session.

## Entry point

`main.rs` is the binary entry; `lib.rs` re-exports every top-level module (and the four subdirs) as `pub mod ...` so the integration tests under `crates/clud-bin/tests/` can link against internals (notably `session::run_raw_pty_pump` and `session::F3Observer`). Production builds resolve each module through the library, so there is exactly one instance of each module in the final binary.

## See also

- Parent crate overview: [`../README.md`](../README.md).
- Top-level project docs and CI matrix: [`../../../CLAUDE.md`](../../../CLAUDE.md).
