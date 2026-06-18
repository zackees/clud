# Launch Setup

Launch setup is the narrow gate for persistent agent-home mutations that happen
before clud starts a backend. It lives in `crates/clud-bin/src/launch_setup.rs`.

## Scope Selector

Interactive TUI launches that explicitly choose a backend with `--claude` or
`--codex` prompt on stderr before the backend starts:

```text
Launch setup scope (Up/Down, Enter):
[x] Session only
[ ] Globally
```

The default is session-only unless `~/.clud/settings.json` already stores a
backend-level global preference, for example:

```json
{
  "launch_setup": {
    "codex": {
      "scope": "global"
    }
  }
}
```

The selector drains any key events that were already pending when it appeared,
so the Enter key used to submit the `clud` command is not reused as the
selector confirmation. Enter accepts the highlighted option, Up selects
session-only, and Down selects global. Selecting global writes the backend's
scope to `~/.clud/settings.json`, so later launches for that backend run global
setup without prompting. Selecting session-only stays scoped to that one launch.

A bare `clud` invocation (no `--claude` or `--codex`), non-interactive backend
launches, piped stdin, one-shot prompt launches (`-p` / `-m`), continuations,
and resumes do not prompt. They use session-only unless a stored backend scope
says `global`. `--dry-run` always uses session-only and does not read or write
the persisted preference. Self-contained maintenance commands exit before
launch setup.

Session-only launches skip persistent setup. They must not create or modify
agent home setup files under `~/.claude`, `~/.codex`, `~/.agents`, or
`~/.clud` as part of harness setup.

## Global Actions

Global setup runs only the selected backend's registered actions:

| Backend | Action | Persistent paths |
|---|---|---|
| Claude | bundled skills | `~/.claude/skills/` |
| Claude | Claude drift skills | `~/.claude/skills/` |
| Codex | bundled skills | `~/.codex/skills/` gated by `~/.codex`; stale clud-managed `~/.agents/skills/` copies are purged |
| Codex | hook timeout normalization | `~/.codex/hooks.json` and `~/.clud/settings.lock` / `settings.json` |
| All | persisted global setup preference | `~/.clud/settings.lock` / `settings.json` |

All setup failures are non-fatal. `main.rs` logs a `[clud] note: ...` line and
continues to build and run the backend `LaunchPlan`.

## Adding an Action

Add a `HarnessSetupAction` implementation in `launch_setup.rs`, give it a
backend, and make `supports(SessionOnly)` false unless the action is proven not
to write persistent agent setup state. Tests should cover both session-only
no-write behavior and selected-backend global behavior.
