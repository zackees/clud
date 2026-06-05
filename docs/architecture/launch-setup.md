# Launch Setup

Launch setup is the narrow gate for persistent agent-home mutations that happen
before clud starts a backend. It lives in `crates/clud-bin/src/launch_setup.rs`.

## Scope Selector

Interactive TUI launches that explicitly choose a backend with `--claude` or
`--codex` prompt on stderr before the backend starts:

```text
[x] Session only
[ ] Globally
```

The default is session-only. Enter accepts the highlighted option, Up selects
session-only, and Down selects global. A bare `clud` invocation (no `--claude`
or `--codex`), non-interactive launches, piped stdin, `--dry-run`, one-shot
prompt launches (`-p` / `-m`), continuations, resumes, and maintenance commands
do not prompt; they use session-only.

Session-only launches skip persistent setup. They must not create or modify
agent home setup files under `~/.claude`, `~/.codex`, `~/.agents`, or
`~/.clud` as part of harness setup.

## Global Actions

Global setup runs only the selected backend's registered actions:

| Backend | Action | Persistent paths |
|---|---|---|
| Claude | bundled skills | `~/.claude/skills/` |
| Claude | Claude drift skills | `~/.claude/skills/` |
| Codex | bundled skills | `~/.agents/skills/` gated by `~/.codex`; stale clud-managed `~/.codex/skills/` copies are purged |
| Codex | hook timeout normalization | `~/.codex/hooks.json` and `~/.clud/settings.lock` / `settings.json` |

All setup failures are non-fatal. `main.rs` logs a `[clud] note: ...` line and
continues to build and run the backend `LaunchPlan`.

## Adding an Action

Add a `HarnessSetupAction` implementation in `launch_setup.rs`, give it a
backend, and make `supports(SessionOnly)` false unless the action is proven not
to write persistent agent setup state. Tests should cover both session-only
no-write behavior and selected-backend global behavior.
