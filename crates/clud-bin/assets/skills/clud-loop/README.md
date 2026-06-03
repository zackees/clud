# clud-loop/

Source of the `/clud-loop` skill shipped inside the `clud` binary. The skill
polyfills Claude-style `/loop` behavior for Codex by keeping the work prompt and
journal in `.clud/loop/LOOP.md`, then driving the existing `clud --codex loop`
engine.

The interval form maps to:

```bash
clud --codex loop --repeat <interval> --loop-count 1 --no-done .clud/loop/LOOP.md
```

The no-interval form asks Codex to delegate to a worker subagent when available,
with the worker reading and updating `.clud/loop/LOOP.md`, and otherwise runs
`clud --codex loop .clud/loop/LOOP.md` directly.

`SKILL.md` is embedded into the `clud` binary at compile time via
`include_str!` from `crates/clud-bin/src/skills.rs`. On every launch,
`skills::ensure_installed` writes it to detected backend skill locations,
including Codex's current `$HOME/.agents/skills/` path and the legacy
`$HOME/.codex/skills/` path when a Codex home exists. Existing user edits are
preserved.
