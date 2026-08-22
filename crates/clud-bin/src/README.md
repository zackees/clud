# src/

The cross-cutting contracts for `clud --codex --harness claude` and
`clud --unified` live in
[`docs/architecture/codex-via-claude.md`](../../../docs/architecture/codex-via-claude.md)
and
[`docs/architecture/unified-gateway.md`](../../../docs/architecture/unified-gateway.md);
this README only maps the source owners.

Entry point and source tree for the `clud-bin` Rust binary. The binary launches
a backend agent (`claude` or `codex`) in YOLO mode, optionally through a PTY,
with first-class support for loop iterations, drag-and-drop, voice input, and a
per-user daemon for backgrounded/detachable sessions. `main.rs` does
cross-cutting startup work (trampoline unlock, console title, launch setup
selection, session-cap registration, GC watch registration) and then hands off to
`runner.rs`, which drives the per-iteration subprocess/PTY launch loop for a
single [`LaunchPlan`]. Submodules under `command/`, `daemon/`, `dnd/`, and
`voice/` carry the bulk of the domain logic; the top-level `.rs` files here are
the orchestration glue and standalone utilities consumed by both `main.rs` and
the integration tests.

## Subdirectories

- [command/](command/README.md) - `LaunchPlan` construction: backend argv
  assembly, YOLO/safe injection, `loop`/`up`/`rebase`/`fix`/`do` prompt synthesis,
  `--repeat` schedule parsing, DONE/BLOCKED contract.
- [daemon/](daemon/README.md) - long-lived session manager for `--detach` /
  `attach` / `list` / `kill` / `logs` / `--repeat`: TCP JSON IPC, per-session
  worker subprocesses, snapshot + log persistence, attach broker.
- [dnd/](dnd/README.md) - drag-and-drop into the terminal: cross-platform
  path-string normalizer plus Windows-only `IDropTarget` adapter with
  per-launch-mode injectors.
- [voice/](voice/README.md) - F3 push-to-talk voice mode: mic capture,
  start/stop cues, `whisper-rs` worker thread, transcript injection into the
  backend PTY.

## Top-Level Modules

Entry and orchestration:

- `main.rs` - process entry: launch clock, trampoline unlock, console title
  stamp + keeper, launch setup selection, large-file guard, session-cap
  registration, GC watch registration, dispatch to runner / daemon / hook-health / GC
  subcommands.
- `lib.rs` - library facade so integration tests under `tests/` can link
  against internals; `main.rs` imports through this rather than re-declaring
  `mod ...`.
- `runner.rs` - per-iteration subprocess- and PTY-mode runner for a single
  `LaunchPlan`; owns child-env construction, stream-json fallback,
  Ctrl-C-aware teardown, and OLE drag-drop registration wiring. The
  backend-aware `child_env_for_backend` reads
  `~/.clud/settings.json::shell.disable_powershell` and, for Claude, injects
  the two undocumented kill-switch env vars `CLAUDE_CODE_USE_POWERSHELL_TOOL=0`
  + `CLAUDE_CODE_GIT_BASH_PATH` (resolved via
  [`shell/`](shell/README.md)) — see issue #447.
- `webterm.rs` - desktop web-terminal launcher: persists the global preference,
  prevents launch recursion with `CLUD_WEBTERM`, and forwards the original
  clud argv to the separately packaged Tauri companion. See
  [`docs/architecture/web-terminal.md`](../../../docs/architecture/web-terminal.md).
- `bridge_log.rs` - issue #772's always-on, failure-only, bounded JSONL writer
  for `~/.clud/state/sessions/<pid>__<epoch>/bridge.jsonl`; test-mode logs use
  the isolated sibling tree `~/.clud/state/test-sessions/`. Buffers complete
  lines across concurrent bridge workers, emits one visible truncation marker,
  and creates no file for a healthy launch.
- `codex_bridge.rs` - issue #626's authenticated, loopback-only HTTP shell and
  #898/#899's unified Claude/Codex/DeepSeek multiplexer: ephemeral listener +
  per-launch bearer, deterministic `/v1/models`, provider catalog routing,
  strict credential isolation, request-time effort mapping, route epochs,
  native Claude token counting, bounded parser/workers/timeouts, authenticated
  context compact/finalize/clear lifecycle controls, and joined shutdown.
  Per-phase header/body deadlines and a per-frame idle timeout preserve chunked
  progressive SSE; see `docs/architecture/unified-gateway.md`.
- `codex_model.rs` - #752's Codex compatibility view over the shared provider
  catalog: the `sol`/`terra`/`luna` aliases and per-model defaults, the
  `<model>@<effort>` parser, and the provider-neutral `Effort` ladder
  restricted to what the family accepts (no `minimal`, no `ultra` --
  re-confirmed against OpenAI's model guidance in #821). Selection rides the
  model string because that is the one field a custom `ANTHROPIC_BASE_URL`
  gateway is allowed to own, so it cannot be dropped in transit the way
  `output_config` can (DD-035). The compound spelling remains a compatibility
  input; current Claude launches advertise provider-scoped discovery IDs and
  carry ordinary effort separately.
- `codex_translate.rs` - pure Anthropic Messages -> OpenAI Responses request
  mapping: typed in/out structs, transcript-order-preserving tool loops,
  auth-mode-dependent system placement, reasoning round-trip, and bounded
  reversible identifiers. Translation is **total** -- droppable Anthropic
  fields are dropped, not rejected (#750, DD-030). The carve-out is a stated
  effort the family does not accept (`output_config.effort`, `@effort`): that
  is a 400 naming the accepted values, because the only alternative is running
  the turn at an effort the user never chose (#821, DD-035).
- `codex_sse.rs` - #627 step 3: `FrameDecoder` (byte-level SSE framing,
  fragmentation/CRLF/heartbeat tolerant) and `StreamTranslator` (Responses
  events -> Anthropic events with monotonic block indices). A tool block never
  opens before its id and name are known. HTTP-free; wired in at step 5.
- `codex_upstream.rs` - #627 step 4: `CredentialSource` (so #629's
  subscription auth reuses translation unchanged) plus a streaming
  `POST /v1/responses` client with connect/read/overall timeouts, bounded
  buffering, cancellation, and a retry policy that stops the moment any byte
  reaches the sink. Builds every outbound header itself, so no downstream
  header can leak upstream. #764 added `UpstreamFailure`/`FailureClass`: the
  error response is read (bounded body prefix, `cf-ray`, `x-request-id`,
  `Retry-After`), classified permanent/transient/unknown, and dropped — the
  class picks the retry budget, backoff is exponential with jitter, and a
  narrow observer reports each attempt/backoff to the bridge forensic log. See
  [`../../../docs/architecture/launch-targets.md`](../../../docs/architecture/launch-targets.md)
  and DD-032.
- `auth.rs` - #898's action-first `clud auth login|status|logout <provider>`
  dispatcher and secret-free aggregate provider status. Hidden compatibility
  aliases retain provider-owned implementations and print an exact replacement.
- `codex_auth.rs` - #629's clud-owned Codex credential implementation: browser
  authorization-code + PKCE callback flow, separate `~/.clud/codex-auth.json`
  store, safe status claims, locked atomic refresh, and token-redacted
  diagnostics. See
  [`../../../docs/architecture/launch-targets.md`](../../../docs/architecture/launch-targets.md).
- `provider_auth.rs` - shared API-key credential implementation (originally
  #877's DeepSeek-only `deepseek_auth.rs`, generalized by #937): hidden terminal
  input, OS-native credential vault adapter parameterized on the vault
  service/account identifiers, injectable in-memory test store, and secret-free
  status/error surfaces.
- `provider_registry.rs` - the `AnthropicCompatProvider` descriptor table
  (#937/#939): per-provider vault identifiers, base URL, CLI flag, optional
  Claude role-model profile, and child-env behavior for providers that speak
  the Anthropic API directly. DeepSeek, Kimi, and OpenRouter share this path;
  adding a provider is primarily a row here plus a `ModelProvider` variant,
  not a new auth or transport stack.
- `codex_pipeline.rs` - #627 step 5: chains translate -> upstream -> SSE into
  one call, plus `MessageAggregator` so a non-streaming request reuses the
  streaming state machine. Owns the downstream status policy — since #764,
  `502` means only a genuine gateway failure, and `TooLarge`/`Cancelled`/
  `Downstream` map to `413`/`499`/`499` instead of borrowing it.
- `failover.rs` - #968: the ordered, cost-labeled failover ladder. A rung is
  either a catalog model (resolved to its provider wire ID) or an ordinary
  Claude model ID forwarded verbatim, because clud owns the synthetic
  provider namespaces while Anthropic owns its own inventory. Descent
  consults `route_health::RouteLedger` and withholds `CostOwner::Metered`
  rungs until consent is recorded. Design:
  `docs/architecture/provider-failover.md`.
- `route_health.rs` - #968: reads an `UpstreamFailure` as a statement about
  its *route* rather than its attempt (`RouteVerdict`: healthy /
  throttled / exhausted / drained / unauthenticated / request-fatal) and
  keeps a launch-scoped `RouteLedger` of which routes can serve and until
  when. Clocks are passed in, never read, so every rule is testable
  without sleeping. Design: `docs/architecture/provider-failover.md`.
- `codex_history.rs` - bounded, in-memory canonical Responses transcript for
  the foreground bridge. It commits only newly pending input plus opaque
  upstream output, evicts at bridge shutdown, performs the two-phase harness
  fallback after failed compaction, and makes validated provider compaction
  replacement and unified provider-route epoch changes atomic; see
  [codex-via-claude.md](../../../docs/architecture/codex-via-claude.md) and
  [unified-gateway.md](../../../docs/architecture/unified-gateway.md).
- `foreground_runtime.rs` - shared foreground lifetime owner and injectable
  subprocess/PTY environment-spawn seam. It conditionally owns the direct Codex
  bridge or unified gateway, applies child-local overlays, emits sanitized
  optional-provider notices, registers launch-scoped authenticated `PreCompact`
  and `SessionStart(clear)` HTTP hooks, and tears the listener down on every
  runner return path. Unified mode enables discovery while preserving Claude
  credentials and ambient session effort; the direct Codex route enables the
  same protocol with a Codex-only catalog and child-local 1.05M context metadata.
- `shell/` - shell-policy plumbing: lazy fetch of a vendored portable Git
  Bash bundle (`shell/git_bash_resolver.rs`) so callers can hand
  `CLAUDE_CODE_GIT_BASH_PATH` to Claude Code without depending on a
  system-wide Git for Windows install. Manifest at
  `vendor/win32/git-bash-bin.toml`; cache at
  `~/.clud/vendor/win32/git-bash-bin-<sha[..12]>/`.
- `startup.rs` - launch-time helpers factored out of `main.rs`: drag-target
  gating (`--no-dnd`, `--dry-run`), session-cap enforcement, Ctrl+C flag
  installer.

CLI surface and backend resolution:

- `args.rs` - `clap` `Args` and `Command` definitions; passthrough for unknown
  flags; subcommand definitions for `loop`, `up`, `rebase`, `fix`, `do`, `gc`, etc.
- `backend.rs` - concrete `Backend`, independent `ModelProvider` /
  `HarnessSelection` resolution, provider `RoutingMode`, process `LaunchMode`,
  PATH lookup, and backend-path resolution. See
  `docs/architecture/launch-targets.md` and
  `docs/architecture/provider-selection.md`.
- `harness_picker.rs` - installed Claude/Codex/DeepSeek discovery plus the
  three-second bare-launch selector and its pure choice/countdown model.
- `provider_catalog.rs` - the single registry mapping stable clud model IDs,
  gateway discovery IDs, provider wire IDs, compatibility aliases, and
  independent effort/context capability metadata.
- `preference.rs` - shared pure typed-choice state machine used by launch
  scope and global settings selectors.
- `subprocess.rs` - single decision point for the Windows `.cmd`/`.bat`
  rewrite (BatBadBat / CVE-2024-24576) via `running-process-core`'s
  `CommandSpec::Shell`.

Console and terminal:

- `console_input.rs` - Windows adapter over
  `running_process::pty::terminal_input::TerminalInputCore` (issues #141 /
  #575): forwards upstream's complete virtual-key translations atomically
  while retaining clud's Shift+Enter-LF and Ctrl+V image policies.
- `console_setup.rs` - RAII guard that enables
  `ENABLE_VIRTUAL_TERMINAL_INPUT` for the lifetime of a PTY session and
  restores the prior console mode on drop; no-op on POSIX.
- `console_title.rs` - stamps `clud <cwd-name>` once on launch and runs a
  background keeper that re-applies the title when downstream OSC 0/2 sequences
  overwrite it.
- `console_title_osc.rs` - stream-resumable OSC 0/2 filter used by the PTY
  output path; re-exported through `console_title` to preserve its call sites.
- `capture.rs` - server-side terminal emulator (`vt100` + `vte` sticky-mode
  sniffer) that lets the daemon synthesize a repaint when a mid-session client
  attaches.
- `session.rs` - raw-PTY pump (`run_raw_pty_pump`), resize handling, F3 voice
  observer hook, OSC-title stripper integration, dropped-path injection on the
  PTY master. Since issue #538 the pump splits output onto a dedicated
  reader thread + stdout-writer thread (`run_output_writer`) so a slow
  terminal flush never delays stdin forwarding — see
  `docs/architecture/session-lifecycle.md` and DD-018.

Loop subsystem (`clud loop`):

- `loop_spec.rs` - task-spec resolver: classifies the positional (GH URL,
  `#42`, file, literal), fetches GH issue/PR bodies via `gh` (curl fallback),
  caches under `.clud/loop/`, locates DONE/BLOCKED marker files.
- `loop_check.rs` - post-iteration DONE/BLOCKED marker check; file-only and
  stdout-scanning variants used by PTY and subprocess paths respectively.
- `loop_artifacts.rs` - durable `<git-root>/.clud/loop/` artifacts:
  `info.json` (`TaskInfo`), `log.txt`, `motivation.md`, and `.gitignore`
  auto-injection.
- `stream_json.rs` - pure renderer for claude's `--output-format stream-json`
  events; turns one JSON event per line into one human-readable progress line
  for subprocess-mode loops.

Process management and GC. The cross-directory story — the two disjoint
reapers, the `(pid, creation_time)` keyspace, daemon-sparing precedence, and
which test tier a change belongs in — lives in
[`docs/architecture/process-reaping.md`](../../../docs/architecture/process-reaping.md).

- `process_identity.rs` - `ProcessIdentity` = PID **plus** OS start time, and the
  comparison every path that stores a PID and acts on it later must go through
  (issue #558). A bare PID is not a stable handle: Windows reissues numbers
  promptly, so "the PID is alive" and "that process is alive" are different
  questions. Records written before the field existed carry
  `UNKNOWN_START_TIME` and fall back to a PID-only comparison.
- `process_scan.rs` - clud's own host **environment** pass on `sysinfo 0.37`
  (#673 Phase 7): `scan_env()` answers both env questions from one snapshot,
  and `DaemonMarkerCache` reads each identity's environment once, ever, over a
  bounded candidate set.
- `process_tree.rs` - best-effort descendant-tree termination via `sysinfo`;
  fixes the multi-second Ctrl+C hang for `clud --codex` on Windows where
  `cmd.exe -> node.exe` would orphan the real child. `TopologySnapshot` is one
  host walk reused across a whole sweep, and re-verifies `(pid, start_time)`
  for **every** target it kills, descendants included (#688).
- `job_orphan_reaper.rs` - Windows foreground Job completion-port tracker.
  The runner registers each backend root by PID + start time; a pure role
  planner recognizes the exact Claude/Codex host, direct tool shells, Git Bash
  handoffs, nested detachments, declared daemons, and the unconditional
  `conhost.exe` exclusion before any automatic client-tree reap (#616).
  Daemon-sparing goes through the `ProcessFacts` seam and its precedence
  ordering (#673 Phase 1a); the tracked keyspace is bounded by one purge sweep
  (Phase 2). The seam itself now lives in `reaper_facts.rs` — this module adds
  only the Job Object signal and its own tool-shell reap reasons.
- `reaper_facts.rs` - the OS-authoritative daemon-sparing signals, shared by
  **both** reapers (#688). `ProcessFacts` / `FactsSnapshot` / `spare_signal`
  are pure data plus one fenced per-platform producer, which is what keeps the
  precedence table unit-testable everywhere. `collect_host_facts` is the
  producer `orphan_reaper` uses; it reports job membership as *unavailable*,
  because there is no Job Object on the `clud slay` / on-exit / daemon-sweep
  path.
- `reap_log.rs` - reaper accounting (`ReapCounters`), buffered mutations-only
  JSONL at `~/.clud/state/sessions/<pid>__<epoch>/reap.jsonl`, and a durable
  five-second `reap-health.json` flight-recorder checkpoint for watchdog-reset
  forensics. It records host scans and adaptive-backoff deferrals without
  synchronous per-event logging.
- `session_registry.rs` - `redb`-backed registry of live `clud` PIDs that caps
  concurrent siblings; `Drop` removes the row, startup GCs dead rows.
- `gc/` - `clud gc list` / `prune` / `purge` / `all` / `reconcile` CLI handlers and
  daemon-watch root derivation. The GC registry and its shared watcher live inside
  the daemon.
- `worktrees.rs` - `--clean-worktrees` (issue #83): enumerates via
  `git worktree list --porcelain`, classifies clean / dirty / unpushed / gone,
  removes safe ones; `--dry-run` faithful.
- `optimize.rs` - `clud optimize rust`: installs/persists soldr defaults and
  writes repo-local `.clud/settings.json` directives.
- `repo_clud_config.rs` - `.clud/settings.json` discovery + parser, both
  repo-level and user-level (two-level merge, repo wins per scalar field).
  Owns the `rust.use_soldr` activation schema (DD-014) and the generic
  `bad_commands` rule schema (DD-016) — repo maintainers add their own
  "bad command → blessed replacement" rules here (e.g. banning bare
  `playwright` in favor of a project's `npm run test:integration`); see
  DD-016 for the full field reference and a copy-pasteable example.
  `bad_commands` concatenates across repo/user levels instead of
  overriding, unlike the scalar `rust.*` fields. Each parsed rule carries a
  non-serialized `RuleSource` (file, layer, `/bad_commands/<index>` slot) so a
  denial can cite exactly which rule fired and where it came from (#525);
  provenance survives merge/dedupe, so a shadowing rule keeps its own origin.
- `block_bad_cmd.rs` - native `cmd-scan` PreToolUse hook binary (formerly
  `block-bad-cmd`; `clud-block-bad-cmd` still ships as a compat binary, see
  `block_bad_cmd_rollout.rs`). Enforces three things per Bash command:
  hardcoded Rust-toolchain rules (`RUST_TOOLS` → `soldr <tool>`); GitHub PR
  waiter rules (`gh ... --watch` / polling loops → `github/pr_merge_watch.py`),
  gated behind the `git.pr_wait_fail_fast` toggle (off by default, see
  `settings_tui.rs`); and the config-driven `bad_commands`/`bad_pipelines`
  engine from `repo_clud_config.rs` (DD-016/DD-017). Also eager-GC-tracks
  `git clone`/`git worktree add` destinations and guards clones outside
  `.extern-repos/` (zackees/clud#532). The scanner splits shell segments and
  unwraps nested shells/`eval`/command-substitution before matching;
  `CLUD_BAD_CMD_OVERRIDE` is the per-rule escape hatch. Config denials cite
  provenance (matched token, rule id, `<file>#/bad_commands/<index>`) in the
  reason and log a structured `bad_cmd_denied` event to
  `~/.clud/tools/hooks/block-bad-cmd.log` (#525). User-facing rule-writing
  guide lives in the root `README.md`. Also enforces `bash.block_cd` via
  `block_bad_cmd_cd.rs` (see below), after the command rules so an
  independently forbidden command reports the stronger reason.
- `block_bad_cmd_cd.rs` - `bash.block_cd` session-cwd pinning (#967 Phase 1,
  DD-047). Scans for `cd`s that would move the *session* cwd — subshells,
  `$(...)`, and nested shells are skipped because they cannot leak — resolves
  the target against the registered roots (Phase 1: the containing repo root)
  and denies per policy. `"auto"` resolves at fire time by classifying every
  hook command in scope for cwd sensitivity: a relative script path pins the
  cwd (strict), all-PATH-binary hooks only block leaving the repo, no hooks
  means no policy. That classifier reads `.claude/settings*.json` and
  `.codex/hooks.json` across **all** events, deliberately not through
  `hook_health::inspect`, which parses only `PreToolUse` — the wedge that
  motivated the issue came from a `Stop` hook. `hook_health` reuses it for the
  broken-`git rev-parse`-prefix warning. Every settings/hook read sits behind
  a word-boundary pre-filter so a command with no `cd` pays one lowercase
  scan.
- `block_bad_cmd_rm_vars.rs` - #963's POSIX/Bash abstract interpreter for
  catastrophic `rm`/`rmdir` operands rooted at `$VAR` or `${VAR}`. It rewrites
  only single, statically proven literal assignments into a complete
  `allow + updatedInput` hook response; unresolved, dynamic, conflicting, or
  root-like values become deterministic denials so unattended agents can retry
  without an approval prompt.
- `settings_tui.rs` - `clud settings`: small cross-platform TUI checkbox menu
  over global boolean settings in `~/.clud/settings.json` (`clud_settings.rs`
  owns persistence). Pure `Menu` state machine + crossterm raw-mode I/O shell,
  same split as `launch_setup.rs`'s `ScopeSelector`. `--list` prints current
  values non-interactively.

Platform glue:

- `trampoline.rs` - Windows-only rename-self-and-copy-back trick so
  `pip install` can always overwrite `Scripts/clud.exe`. No-op on POSIX.
- `win_creation_flags.rs` - `invisible_helper_creationflags()` returns
  `CREATE_NO_WINDOW` on Windows for daemon-helper spawns; `0` elsewhere so call
  sites stay portable.
- `large_file_guard.rs` - startup-time guard that warns about source files
  large enough to choke agents (issue #132). On a git repo the primary path is
  now the in-process **index pass** (`large_file_guard/index_pass.rs`, issue
  #556): `gix-index` mmap-parses `.git/index` (resolving the `.git`-file
  `gitdir:` indirection so a linked worktree reports against its own index) and
  reports tracked source files straight from the index's cached stat sizes —
  no tree walk, ~ms instead of ~240-400 ms. Entries whose cached size is
  untrustworthy (racily-clean, recorded as 0) get one targeted `stat` each. The
  two failure modes route differently, and the distinction is load-bearing:
  **no index** (not a git repo, broken `gitdir:` indirection) falls to the
  original `ignore`-crate parallel walker under its hard 1 s deadline, while a
  **corrupt/unparseable index** first takes one killable
  `git ls-files --debug` (`large_file_guard/ls_files_pass.rs`), which still
  reads cached sizes and still never touches the object database. Collapsing
  the two would give up the whole win on precisely the repos most likely to be
  large. Untracked-file coverage on the launch path is deferred to the
  daemon-side pass 2 (#551). Why the index — and why never
  `--format='%(objectsize)'` / `ls-tree`, which are 3-12x slower and can
  trigger a network fetch on a partial clone:
  [DD-022](../../../docs/DESIGN_DECISIONS.md#dd-022-the-large-file-guard-reads-the-git-index-in-process-not-the-worktree).
- `path_norm.rs` - fbuild/zccache-style `NormalizedPath` and separator-safe
  path-string helpers for cross-platform path keys, serialization, and
  executable names received from another OS.
- `launch_setup.rs` - session-only/global setup selector plus
  selected-backend persistent setup actions for skills and Codex hook
  normalization.

Skills and hooks:

- `skills.rs` - the only skill installer, over the only skill source tree
  (`assets/skills/`). Bundles slash-command skills via `include_str!` and
  installs them during global launch setup for the selected backend
  (`.claude/skills/`, Codex `.codex/skills/` gated on `.codex`) only when the
  backend home already exists. Writes a missing file; never touches a file the
  user has taken ownership of (`managed-by: clud` marker stripped); refreshes a
  clud-managed copy that diverges from the bundle modulo whitespace; writes
  nothing when the installed copy is current. Purges stale clud-managed copies
  from `.agents/skills/` and retired names in `PURGED_BUNDLED_SKILLS` from every
  backend's skills dir. See
  [DD-039](../../../docs/DESIGN_DECISIONS.md#dd-039-single-skill-installer-over-a-single-source-tree).
- `hook_health/` - `PreToolUse` hook parity diagnostics and `--fix-hooks`
  remediation.
- `block_bad_cmd_rollout.rs` - startup health/migration for the native
  `clud-block-bad-cmd` helper: stale install warning plus exact old hook
  command rewrites when the helper is available.
- `codex_hook_normalize.rs` - issue #234: idempotent Codex global-setup pass
  that bumps any `~/.codex/hooks.json` handler `timeout: 5` to `30`
  (`~/.clud/settings.lock` fs4 guard, green status line on change).

Diagnostics and misc:

- `cpu_banner.rs` - issue #466: foreground CPU-burn banner. Spawns one
  background `sysinfo` sampler that ticks every 2 s, sums `cpu_usage()` +
  `memory()` over the parent-PID subtree rooted at our originator, and emits
  `[clud] cpu N % · X.Y / Z cores · …` to stderr when subtree CPU crosses
  `max(50 %, 0.20 × num_cpus × 100 %)` for 3 sustained ticks. Hysteretic
  drop-out at 0.7×; 30 s heartbeat while sustained; clear-banner only after
  ≥ 60 s episodes. Wired into `runner::run_plan_subprocess` and
  `runner::run_plan_pty` via a `BannerWatcher` whose `Drop` joins the
  thread. Suppressed by `--no-cpu-banner`, `--dry-run`, `--detach`,
  `--detachable`, `--repeat`, and `[foreground.cpu_banner] enabled = false`
  in `~/.clud/settings.json`. Slice of #463 (`clud top`).
  Issue #709: the full-system subtree rebuild (the dominant cost — #553
  measured 225 ms loaded, 2.09 s saturated) backs off 30 s → 120 s via
  `RebuildCadence` while the subtree stays under `REBUILD_QUIET_PCT`. Any
  activity resets the cadence *and* forces an immediate rebuild on the next
  tick, so a busy session is never sampled against a stale pid list — the
  backoff is only ever paid for by an idle one.
- `wedge_watchdog.rs` - issue #541: detects a wedged backend TUI (one thread
  pinned ≥ 90% of one core in user-mode with near-zero process IO-write bytes,
  sustained for `DEFAULT_REQUIRED_STREAK` × `DEFAULT_TICK` ≈ 90 s). Pure
  `WedgeDetector` state machine (`Healthy` / `Suspect{streak}` / `Wedged`) is
  platform-free and exhaustively unit tested; the Windows-only sampler walks
  the monitored pid's process subtree via `Toolhelp32` + `GetThreadTimes` +
  `GetProcessIoCounters`. On `Wedged`, `WedgeWatchdog` (same
  `Drop`-joins-thread shape as `BannerWatcher` above) prints one rate-limited
  stderr warning naming the backend and a `codex resume`-style recovery
  hint, and logs the measured signature via `verbose_log`. Wired into
  `runner::run_plan_subprocess` and `runner::run_plan_pty`. No-op on
  non-Windows. E2E probes against real spinning threads live in
  `tests/wedge_watchdog_e2e.rs` (ignored; run manually).
  Issue #709: a healthy tick no longer pays for the host-wide thread
  enumeration. `subtree_could_hide_a_hot_thread` compares each process's
  *user-mode* delta against `GATE_USER_PCT_THRESHOLD` first — a thread's user
  time can never exceed its process's, so a cool subtree provably has no hot
  thread and the `TH32CS_SNAPTHREAD` walk (which enumerates **every thread on
  the host** before filtering) is skipped. The gate fails open on any
  unanswerable input, and a gated tick still reports an explicitly *healthy*
  sample rather than `None` — returning `None` would leave a partial wedge
  streak standing across an idle stretch, since the loop treats it as "no
  observation". `descendants_of` also carries a visited set: a PID-reuse cycle
  in the Toolhelp parent graph previously made the walk non-terminating.
- `verbose_log.rs` - launch-clock + opt-in file logging
  (`CLUD_VERBOSE_LOG_DIR`); `log()` writes timestamped lines to the per-launch
  log file.
- `crash_report.rs` - process panic hook + native crash handler installed
  from `main.rs` (role=`foreground`), `daemon/server.rs::run_daemon`
  (role=`daemon`), and `daemon/worker.rs::run_worker` (role=`worker`).
  Both panic-driven and native-crash-driven (`crash-handler` crate;
  SIGSEGV/SIGBUS/SIGILL/SIGFPE/SIGABRT on Unix; structured exceptions on
  Windows) reports share one writer producing JSON records with backtrace
  under `~/.clud/state/crashes/<unix_ms>-<role>-<pid>.json`, prunes to
  the 50 most recent, and surfaces a one-line stderr notice on the next
  launch when a new report appears (plus a follow-up "backtrace appears
  unsymbolicated; run `clud symbols verify`" line when the new report
  has zero `at FILE:LINE` frames — #374 PR 3). `install_native()` is
  idempotent — the hook is installed once per process; re-calling only
  updates the role tag. Native install **does not attach a
  SIGINT/CTRL_C_EVENT handler**, leaving the existing
  `startup::install_ctrl_c_flag` / `ctrl_c_track` (#372) path
  authoritative for Ctrl-C.
- `symbols.rs` - `clud symbols` / `clud symbols install` / `clud symbols
  verify [--all]` subcommand handler. With `debug = "line-tables-only"`
  embedded in every build (#374 PR 1), no sidecar files need to be
  fetched; the verifier confirms the running binary can resolve recent
  crash reports' `at FILE:LINE` frames and exits 0/1 accordingly. The
  bare `clud symbols` form prints a five-line summary. Self-contained
  maintenance command; dispatched from `main.rs` before any backend
  resolution. See `docs/architecture/crash-reports.md`.
- `wasm.rs` - `wasmi`-based runner that loads a WASM module, registers a
  minimal `host.log` import, invokes a named export, and propagates the integer
  exit code.

Quick lookup, which file owns a given subcommand:

- `clud loop ...` -> `command::build_launch_plan_for_target` (resolved
  provider/harness, prompt, and markers) +
  `loop_spec` (task resolution) + `loop_artifacts` (artifact files) +
  `runner.rs` (iteration loop) + `loop_check` (DONE/BLOCKED scan).
- `clud --detach`, `clud attach`, `clud list`, `clud kill`, `clud logs` -> all
  in `daemon/` (dispatched from `daemon::handle_special_command`).
- `clud gc list` / `prune` / `purge` / `all` / `reconcile` -> `gc/cli.rs` (CLI handlers) talking to
  `daemon/gc_service.rs` (registry owner inside the always-on `__daemon`).
- `clud grind [url]` -> `grind.rs` resolves the repo's `origin` remote to its
  forge issues page (GitHub `<repo>/issues`, GitLab `<repo>/-/issues`), prints
  the green notice, and keeps the `Grind` command (issue #897 — it no longer
  rewrites to `Do`). `command/builder.rs` then builds the `/loop` prompt via
  `build_grind_prompt` and arms the loop subsystem's DONE/BLOCKED markers, so
  grinding iterates one issue at a time instead of running `/goal`'s
  single-shot flow. An explicit URL is passed through verbatim.
- `clud --clean-worktrees` -> `worktrees.rs`.
- `clud optimize rust` -> `optimize.rs`.
- `clud --fix-hooks` -> `hook_health/`.
- `clud settings [--list]` -> `settings_tui.rs`.

## Cross-Cutting Subsystems

Subsystems that span multiple files have their own topic docs under
`docs/architecture/`:

- **Loop subsystem** (`command/`, `loop_spec`, `loop_check`, `loop_artifacts`,
  `stream_json`, `runner`) -> [docs/architecture/loop-subsystem.md](../../../docs/architecture/loop-subsystem.md)
- **Daemon IPC** (everything under `daemon/`) -> [docs/architecture/daemon-ipc.md](../../../docs/architecture/daemon-ipc.md)
- **Session lifecycle** (`session`, `console_*`, `capture`, `dnd` injection,
  `voice` hooks) -> [docs/architecture/session-lifecycle.md](../../../docs/architecture/session-lifecycle.md)
- **Skill system** (`skills`, `assets/skills/`) -> [docs/architecture/skill-system.md](../../../docs/architecture/skill-system.md)
- **Launch setup** (`launch_setup`, selected-backend persistent setup) -> [docs/architecture/launch-setup.md](../../../docs/architecture/launch-setup.md)
- **GC and registry** (`gc`, `daemon/gc_service`, `session_registry`,
  `worktrees`) -> [docs/architecture/gc-and-registry.md](../../../docs/architecture/gc-and-registry.md)
- **Windows quirks** (`trampoline`, `subprocess` BatBadBat, `console_*`,
  foreground Job Object shell-orphan reaping, `dnd`, `win_creation_flags`,
  `voice` ARM carveout) -> [docs/architecture/windows-quirks.md](../../../docs/architecture/windows-quirks.md)
- **Launch plan** (`command/types::LaunchPlan` + all consumers) -> [docs/architecture/launch-plan.md](../../../docs/architecture/launch-plan.md)
- **Launch targets** (provider/harness resolution + sticky settings) -> [docs/architecture/launch-targets.md](../../../docs/architecture/launch-targets.md)
- **Provider selection** (routing mode + provider-neutral model registry) -> [docs/architecture/provider-selection.md](../../../docs/architecture/provider-selection.md)
- **Unified gateway** (`codex_bridge`, `codex_history`, `foreground_runtime`,
  `provider_catalog`, `auth`) -> [docs/architecture/unified-gateway.md](../../../docs/architecture/unified-gateway.md)

Non-obvious design choices (single `LaunchPlan`, `lib.rs` as the only
`mod ...` site, cooperative Ctrl+C, redb single-owner) have ADRs in
[docs/DESIGN_DECISIONS.md](../../../docs/DESIGN_DECISIONS.md).

## Entry Point

`main.rs` is the binary entry; `lib.rs` re-exports every top-level module (and
the four subdirs) as `pub mod ...` so integration tests under
`crates/clud-bin/tests/` can link against internals. See
[DD-007](../../../docs/DESIGN_DECISIONS.md#dd-007-librs-is-the-only-place-that-declares-modules-mainrs-imports-through-clud)
for why the single-instantiation pattern matters.

## See Also

- Parent crate overview: [`../README.md`](../README.md).
- Top-level project docs and CI matrix: [`../../../CLAUDE.md`](../../../CLAUDE.md).
