# clud Architecture

Index of subsystem architecture docs. Each file is self-contained for one
cross-cutting concept; per-directory READMEs link here instead of
re-explaining.

## Subsystem Docs

| Document | Lines | What it covers |
|---|---|---|
| [architecture/loop-subsystem.md](architecture/loop-subsystem.md) | ~250 | `clud loop`: task resolution, plan synthesis, iteration run, DONE/BLOCKED marker contract, artifact rollover, repeat scheduling |
| [architecture/daemon-ipc.md](architecture/daemon-ipc.md) | ~250 | Always-on clud daemon hosting session ops + GC: TCP JSON IPC, daemon/worker re-entry model, snapshot persistence, attach broker |
| [architecture/session-lifecycle.md](architecture/session-lifecycle.md) | ~300 | PTY session pump, console mode setup, OSC title keeper, capture for attach, drag-drop and voice injection points |
| [architecture/skill-system.md](architecture/skill-system.md) | ~200 | Skill bundling (`include_str!`), the single `skills.rs` installer over `assets/skills/`, the four-state install contract, selected-backend global setup |
| [architecture/launch-setup.md](architecture/launch-setup.md) | ~70 | Session-only vs global launch setup, persistent setup actions, selected-backend gating |
| [architecture/gc-and-registry.md](architecture/gc-and-registry.md) | ~250 | always-on `clud __daemon` single-owner redb model, session cap registry, worktree scanner, GC subcommands |
| [architecture/windows-quirks.md](architecture/windows-quirks.md) | ~300 | Windows-only platform code: trampoline, BatBadBat `.cmd` rewrite, console modes, Shift+Enter key translation, `IDropTarget`, `CREATE_NO_WINDOW`, ARM whisper carveout |
| [architecture/launch-plan.md](architecture/launch-plan.md) | ~180 | `LaunchPlan` as the single source of truth: construction, consumers, `--dry-run` JSON |
| [architecture/launch-targets.md](architecture/launch-targets.md) | ~540 | Independent model-provider and harness resolution, sticky settings, foreground bridge lifecycle, compatibility; DeepSeek direct provider (credential trust boundary, preflight, child overlay, no bridge) |
| [architecture/provider-selection.md](architecture/provider-selection.md) | ~120 | #900's compatible launch grammar, direct-vs-unified routing mode, provider-neutral model catalog, modifier normalization, and plan/repeat propagation |
| [architecture/unified-gateway.md](architecture/unified-gateway.md) | ~130 | #898/#899's authenticated foreground multiplexer: version gate, discovery, provider routing, route epochs, token counting, credential isolation, and cross-provider effort contract |
| [architecture/codex-via-claude.md](architecture/codex-via-claude.md) | ~120 | Canonical cross-route ownership: resolution, bridge protocol, credentials, security boundary, rollback |
| [architecture/crash-reports.md](architecture/crash-reports.md) | ~110 | Panic-hook + native crash handler + `clud symbols` verifier: JSON schema, embed-line-tables-everywhere choice, opportunistic-verify model (#374) |
| [architecture/process-reaping.md](architecture/process-reaping.md) | ~200 | The two disjoint reapers, the `(pid, creation_time)` keyspace and its single purge sweep, daemon-sparing by OS signal (marker second, whitelist last), the cooperative-marker caveat, and which test tier a reaper change belongs in |
| [architecture/ci.md](architecture/ci.md) | ~380 | Build-once/run-everywhere CI: per-triple cross-compilation on Linux, test bundles, exec runners with no toolchain, target tiers, release-profile containment |
| [architecture/test-runtime-memory.md](architecture/test-runtime-memory.md) | ~220 | **Design proposal (#405, not yet implemented):** `.clud/`-local test-runtime histogram — append-only JSONL over redb/SQLite and why, raw `(duration, cpu_load)` with query-time normalization, count-based compaction, and the run-all-vs-targeted recommendation policy |

## Quick Reference

- **"How do I measure idle daemon and client cost?"** -> [idle CPU benchmark](../bench/idle_cpu/README.md)

- **"How does `clud loop` decide when to stop?"** -> [loop-subsystem.md](architecture/loop-subsystem.md)
- **"How do `attach` / `list` / `kill` talk to the daemon?"** -> [daemon-ipc.md](architecture/daemon-ipc.md)
- **"What happens between Ctrl-D and process exit in a PTY session?"** -> [session-lifecycle.md](architecture/session-lifecycle.md)
- **"When does a bundled skill get written into my home?"** -> [skill-system.md](architecture/skill-system.md)
- **"When does clud write agent setup files?"** -> [launch-setup.md](architecture/launch-setup.md)
- **"Why is `~/.clud/data.redb` behind a daemon?"** -> [gc-and-registry.md](architecture/gc-and-registry.md)
- **"Why does CI compile on Linux but test on macOS/Windows?"** -> [ci.md](architecture/ci.md)
- **"What is clud allowed to kill, and where does my reaper test go?"** -> [process-reaping.md](architecture/process-reaping.md)
- **"Why does Windows do X differently?"** -> [windows-quirks.md](architecture/windows-quirks.md)
- **"Where does the argv that clud runs come from?"** -> [launch-plan.md](architecture/launch-plan.md)
- **"How do provider and harness preferences resolve?"** -> [launch-targets.md](architecture/launch-targets.md)
- **"Which model ID is stable, and how do effort/context reach a worker?"** -> [provider-selection.md](architecture/provider-selection.md)
- **"How does one Claude session switch safely among providers?"** -> [unified-gateway.md](architecture/unified-gateway.md)
- **"How does Codex run through Claude, and how do I roll it back?"** -> [codex-via-claude.md](architecture/codex-via-claude.md)
- **"What happens when clud crashes, and how do I read the report?"** -> [crash-reports.md](architecture/crash-reports.md)
- **"Should I run all the tests or just some?"** -> [test-runtime-memory.md](architecture/test-runtime-memory.md) *(design proposal, #405)*

See also: [DESIGN_DECISIONS.md](DESIGN_DECISIONS.md) for rationale behind the
choices these subsystems embody.

Release-manager evidence for the optional cross-route is tracked separately in
[release-codex-via-claude.md](release-codex-via-claude.md).
