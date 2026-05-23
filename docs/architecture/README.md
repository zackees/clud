# Architecture Documents

Subsystem-level architecture for clud, split by topic so each doc is self-contained.

| Document | Covers |
|---|---|
| [loop-subsystem.md](loop-subsystem.md) | `clud loop`: spec → plan → iteration → marker → artifact cycle |
| [daemon-ipc.md](daemon-ipc.md) | Always-on clud daemon hosting session ops + GC, TCP JSON IPC, worker re-entry, attach broker |
| [session-lifecycle.md](session-lifecycle.md) | PTY pump, console setup, title keeper, capture/replay, input injection |
| [skill-system.md](skill-system.md) | Skill bundling and the dual-installer model (multi-backend vs Claude-overwrite) |
| [gc-and-registry.md](gc-and-registry.md) | always-on `clud __daemon` single-owner redb, session cap, worktree scanner, GC subcommands |
| [windows-quirks.md](windows-quirks.md) | Windows-only platform code consolidated in one place |
| [launch-plan.md](launch-plan.md) | `LaunchPlan` as the single source of truth for what clud actually runs |

See the parent [ARCHITECTURE.md](../ARCHITECTURE.md) for the quick-reference table and [DESIGN_DECISIONS.md](../DESIGN_DECISIONS.md) for the "why" behind these designs.
