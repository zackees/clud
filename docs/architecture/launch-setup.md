# Launch Setup

Launch setup is the narrow gate for persistent agent-home mutations that happen
before clud starts a backend. It lives in `crates/clud-bin/src/launch_setup.rs`.

## Scope Selector

Interactive TUI launches that explicitly choose a backend with `--claude` or
`--codex` prompt on stderr before the backend starts:

```text
[x] Session only
[ ] Globally
[ ] Globally + clud memory (recommended)
```

The default is session-only when the `clud-memory` MCP block is already
registered, and "Globally + clud memory (recommended)" when memory is not yet
configured (probed via `mcp_config::memory_already_registered`). Enter accepts
the highlighted option; Up/Down navigate (with wrap-around — Up from row 0
selects row 2, Down from row 2 selects row 0). A bare `clud` invocation (no
`--claude` or `--codex`), non-interactive launches, piped stdin, `--dry-run`,
one-shot prompt launches (`-p` / `-m`), continuations, resumes, and maintenance
commands do not prompt; they use session-only.

Session-only launches skip persistent setup. They must not create or modify
agent home setup files under `~/.claude`, `~/.codex`, `~/.agents`, or
`~/.clud` as part of harness setup.

## Global Actions

Global setup runs only the selected backend's registered actions. Actions whose
`supports()` returns true for the current scope run in registration order;
existing actions opt in via `LaunchSetupScope::runs_global_actions()`, while
the two memory-registration actions intentionally gate on
`LaunchSetupScope::GlobalWithMemory` only.

| Scope | Backend | Action | Persistent paths |
|---|---|---|---|
| Global / GlobalWithMemory | Claude | bundled skills | `~/.claude/skills/` |
| Global / GlobalWithMemory | Claude | Claude drift skills | `~/.claude/skills/` |
| Global / GlobalWithMemory | Codex | bundled skills | `~/.agents/skills/` gated by `~/.codex`; stale clud-managed `~/.codex/skills/` copies are purged |
| Global / GlobalWithMemory | Codex | hook timeout normalization | `~/.codex/hooks.json` and `~/.clud/settings.lock` / `settings.json` |
| GlobalWithMemory only | Claude | memory MCP registration (#265) | `~/.claude.json` (`mcpServers.clud-memory`) |
| GlobalWithMemory only | Codex | memory MCP registration (#265) | `~/.codex/config.toml` (`[mcp_servers.clud-memory]`) |
| GlobalWithMemory only | Claude | memory hook registration (#265) | `~/.claude/settings.json` (`hooks.{SessionStart,UserPromptSubmit,PostToolUse,Stop}`) |
| GlobalWithMemory only | Codex | memory hook registration (#265) | `~/.codex/hooks.json` (same four events) |

All setup failures are non-fatal. `main.rs` logs a `[clud] note: ...` line and
continues to build and run the backend `LaunchPlan`.

### Memory registration idempotency

The two memory-registration actions are **idempotent** and **refuse to clobber
hand-edits**. The managed entries carry a `_clud_managed: true` field (JSON) or
a `# managed-by: clud-memory` lead comment (TOML). Re-running on an up-to-date
file is a no-op. When a `clud-memory` key exists *without* the marker, the
helper returns `Error::UserDefined { path, key }`; the action surfaces a
`[clud] note: refusing to overwrite ...` line and continues without writing.
The user's escape hatch is to rename the conflicting block or hand-edit the
marker in, then re-run `clud --setup`.

Writes are atomic (temp-file + rename in the same directory) and serialized by
a sibling `~/.clud/memory-{claude,codex}-{mcp,hooks}.lock` advisory file lock
(same `fs4` pattern used by `codex_hook_normalize`).

## Adding an Action

Add a `HarnessSetupAction` implementation in `launch_setup.rs`, give it a
backend, and make `supports(SessionOnly)` false unless the action is proven not
to write persistent agent setup state. Tests should cover both session-only
no-write behavior and selected-backend global behavior.
