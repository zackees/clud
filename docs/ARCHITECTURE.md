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
| [architecture/memory.md](architecture/memory.md) | ~80 | Agent-memory storage + hybrid search foundation: SqliteStore (rusqlite + sqlite-vec), LexicalIndex (tantivy BM25), RRF fusion, on-disk layout. Stub for sibling sub-issues under META #255 |

## Quick Reference

- **"How does `clud loop` decide when to stop?"** -> [loop-subsystem.md](architecture/loop-subsystem.md)
- **"How do `attach` / `list` / `kill` talk to the daemon?"** -> [daemon-ipc.md](architecture/daemon-ipc.md)
- **"What happens between Ctrl-D and process exit in a PTY session?"** -> [session-lifecycle.md](architecture/session-lifecycle.md)
- **"Why are there two skill installers?"** -> [skill-system.md](architecture/skill-system.md)
- **"When does clud write agent setup files?"** -> [launch-setup.md](architecture/launch-setup.md)
- **"Why is `~/.clud/data.redb` behind a daemon?"** -> [gc-and-registry.md](architecture/gc-and-registry.md)
- **"Why does Windows do X differently?"** -> [windows-quirks.md](architecture/windows-quirks.md)
- **"Where does the argv that clud runs come from?"** -> [launch-plan.md](architecture/launch-plan.md)
- **"How does agent memory persist?"** -> [memory.md](architecture/memory.md)

See also: [DESIGN_DECISIONS.md](DESIGN_DECISIONS.md) for rationale behind the
choices these subsystems embody.
