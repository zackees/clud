# clud-loop/

Source of the `/clud-loop` skill shipped inside the `clud` binary. The skill
polyfills Claude-style `/loop` behavior for Codex by keeping a compact parent
ledger in `.clud/loop/LOOP.md` and running bounded in-chat iterations from the
current Codex thread.

Foreground mode keeps the main Codex agent as the orchestrator. Subagents, when
available, are bounded workers only: they receive strict no-recursion packets
and return structured summaries for the parent to validate and write back to
the ledger.

Scheduled mode should prefer a Codex same-thread automation that runs one
bounded iteration per wake-up. The old process-runner path remains available
only when the user explicitly asks for legacy external automation.

The legacy interval form maps to:

```bash
clud --codex loop --repeat <interval> --loop-count 1 --no-done .clud/loop/LOOP.md
```

The legacy one-shot external form maps to:

```bash
clud --codex loop .clud/loop/LOOP.md
```

`SKILL.md` is embedded into the `clud` binary at compile time via
`include_str!` from `crates/clud-bin/src/skills.rs`. On every launch,
`skills::ensure_installed` writes it to detected backend skill locations
(`$HOME/.claude/skills/` for Claude, `$HOME/.codex/skills/` for Codex).
Existing user edits are preserved.
