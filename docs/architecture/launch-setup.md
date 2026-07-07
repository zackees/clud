# Launch Setup

Launch setup is the narrow gate for persistent agent-home mutations that happen
before clud starts a backend. It lives in `crates/clud-bin/src/launch_setup.rs`.

## Scope Selector

Interactive TUI launches that explicitly choose a backend with `--claude` or
`--codex` prompt on stderr before the backend starts:

```text
Launch setup scope
  Up/Down move, Enter select, Esc session-only
> [x] Session only   this launch
  [ ] Globally       remember this backend
```

The selector stays in the normal terminal scrollback: no alternate screen, no
graphics mode. It hides the hardware cursor while active and uses the visible
`>` marker as the selection cursor. The default is session-only unless
`~/.clud/settings.json` already stores a backend-level global preference, for
example:

```json
{
  "backend": {
    "default": "codex"
  },
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
session-only, Down selects global, and Esc chooses session-only. `j`/`k` mirror
Down/Up for terminals where those keys are more convenient. Ctrl-C/Ctrl-D abort
the launch with exit code 130.

Selecting global on an explicit `--codex` or `--claude` launch writes two
settings to `~/.clud/settings.json`: the selected backend becomes
`backend.default`, and that backend's setup scope becomes `global`. After
`clud --codex` is selected globally, later bare `clud` launches use Codex until
the user runs `clud --claude` and selects global. Selecting session-only stays
scoped to that one launch and does not rewrite either setting.

When an explicit backend flag differs from the stored `backend.default`, clud
shows the selector even if that backend already has a stored global setup
scope. This keeps temporary `clud --codex` / `clud --claude` launches from
silently changing the default; only a fresh `Globally` selection changes it.

A bare `clud` invocation (no `--claude` or `--codex`), non-interactive backend
launches, piped stdin, one-shot prompt launches (`-p` / `-m`), continuations,
and resumes do not prompt. They use the stored default backend when present and
use session-only unless that backend's stored setup scope says `global`.
`--dry-run` ignores stored backend and setup preferences: explicit backend
flags still win, otherwise it uses the built-in Claude/session-only defaults.
Self-contained maintenance commands exit before launch setup.

Session-only launches skip persistent setup. They must not create or modify
agent home setup files under `~/.claude`, `~/.codex`, `~/.agents`, or
`~/.clud` as part of harness setup. Bundled Python tools under
`~/.clud/tools/` are outside this launch-setup selector: normal foreground
startup, daemon startup, and `clud tool run` refresh clud-managed copies by
comparing the installed file with the embedded `BUNDLED_TOOLS` body and
replacing divergent managed copies.

## Global Actions

Global setup runs only the selected backend's registered actions:

| Backend | Action | Persistent paths |
|---|---|---|
| Claude | bundled skills | `~/.claude/skills/` |
| Claude | Claude drift skills | `~/.claude/skills/` |
| Codex | bundled skills | `~/.codex/skills/` gated by `~/.codex`; stale clud-managed `~/.agents/skills/` copies are purged |
| Codex | hook timeout normalization | `~/.codex/hooks.json` and `~/.clud/settings.lock` / `settings.json` |
| All | persisted global setup preference | `~/.clud/settings.lock` / `settings.json` |
| All | persisted default backend | `~/.clud/settings.lock` / `settings.json` |

All setup failures are non-fatal. `main.rs` logs a `[clud] note: ...` line and
continues to build and run the backend `LaunchPlan`.

Bundled Python tools are deliberately not registered as launch-setup actions.
They are backend-agnostic clud commands, so their stale-copy replacement runs
on non-dry-run foreground startup even when the selected launch setup scope is
session-only.

The native `clud-block-bad-cmd` rollout has a similarly narrow foreground
startup repair outside the launch-setup selector: clud warns when an installed
layout has `clud`/`clud-shim` but lacks the native helper, and, when hook
auto-repair is enabled, rewrites only exact old
`clud tool run hooks/block-bad-cmd.py` hook commands to `clud-block-bad-cmd`
after the helper is resolvable on PATH. Non-exact user hook commands are left
alone.

## Adding an Action

Add a `HarnessSetupAction` implementation in `launch_setup.rs`, give it a
backend, and make `supports(SessionOnly)` false unless the action is proven not
to write persistent agent setup state. Tests should cover both session-only
no-write behavior and selected-backend global behavior.
