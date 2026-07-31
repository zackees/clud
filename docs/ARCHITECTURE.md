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
| [architecture/skill-system.md](architecture/skill-system.md) | ~200 | Skill bundling (`include_str!`), dual-installer model (`skills.rs` vs `skill_install.rs`), selected-backend global setup |
| [architecture/launch-setup.md](architecture/launch-setup.md) | ~70 | Session-only vs global launch setup, persistent setup actions, selected-backend gating |
| [architecture/gc-and-registry.md](architecture/gc-and-registry.md) | ~250 | always-on `clud __daemon` single-owner redb model, session cap registry, worktree scanner, GC subcommands |
| [architecture/windows-quirks.md](architecture/windows-quirks.md) | ~300 | Windows-only platform code: trampoline, BatBadBat `.cmd` rewrite, console modes, Shift+Enter key translation, `IDropTarget`, `CREATE_NO_WINDOW`, ARM whisper carveout |
| [architecture/launch-plan.md](architecture/launch-plan.md) | ~180 | `LaunchPlan` as the single source of truth: construction, consumers, `--dry-run` JSON |
| [architecture/crash-reports.md](architecture/crash-reports.md) | ~110 | Panic-hook + native crash handler + `clud symbols` verifier: JSON schema, embed-line-tables-everywhere choice, opportunistic-verify model (#374) |
| [architecture/process-reaping.md](architecture/process-reaping.md) | ~200 | The two disjoint reapers, the `(pid, creation_time)` keyspace and its single purge sweep, daemon-sparing by OS signal (marker second, whitelist last), the cooperative-marker caveat, and which test tier a reaper change belongs in |
| [architecture/ci.md](architecture/ci.md) | ~380 | Build-once/run-everywhere CI: per-triple cross-compilation on Linux, test bundles, exec runners with no toolchain, target tiers, release-profile containment |
| [architecture/test-runtime-memory.md](architecture/test-runtime-memory.md) | ~220 | **Design proposal (#405, not yet implemented):** `.clud/`-local test-runtime histogram — append-only JSONL over redb/SQLite and why, raw `(duration, cpu_load)` with query-time normalization, count-based compaction, and the run-all-vs-targeted recommendation policy |

## Quick Reference

- **"How do I measure idle daemon and client cost?"** -> [idle CPU benchmark](../bench/idle_cpu/README.md)

- **"How does `clud loop` decide when to stop?"** -> [loop-subsystem.md](architecture/loop-subsystem.md)
- **"How do `attach` / `list` / `kill` talk to the daemon?"** -> [daemon-ipc.md](architecture/daemon-ipc.md)
- **"What happens between Ctrl-D and process exit in a PTY session?"** -> [session-lifecycle.md](architecture/session-lifecycle.md)
- **"Why are there two skill installers?"** -> [skill-system.md](architecture/skill-system.md)
- **"When does clud write agent setup files?"** -> [launch-setup.md](architecture/launch-setup.md)
- **"Why is `~/.clud/data.redb` behind a daemon?"** -> [gc-and-registry.md](architecture/gc-and-registry.md)
- **"Why does CI compile on Linux but test on macOS/Windows?"** -> [ci.md](architecture/ci.md)
- **"What is clud allowed to kill, and where does my reaper test go?"** -> [process-reaping.md](architecture/process-reaping.md)
- **"Why does Windows do X differently?"** -> [windows-quirks.md](architecture/windows-quirks.md)
- **"Where does the argv that clud runs come from?"** -> [launch-plan.md](architecture/launch-plan.md)
- **"What happens when clud crashes, and how do I read the report?"** -> [crash-reports.md](architecture/crash-reports.md)
- **"Should I run all the tests or just some?"** -> [test-runtime-memory.md](architecture/test-runtime-memory.md) *(design proposal, #405)*

See also: [DESIGN_DECISIONS.md](DESIGN_DECISIONS.md) for rationale behind the
choices these subsystems embody.
