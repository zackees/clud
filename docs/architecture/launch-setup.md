# Launch Setup

Launch setup is the narrow gate for persistent agent-home mutations that happen
before clud starts a backend. It lives in `crates/clud-bin/src/launch_setup.rs`.

## Scope Selector

Interactive TUI launches that explicitly choose a model provider or harness
with `--claude`, `--codex`, or `--harness` prompt on stderr before the harness
starts:

```text
Launch setup scope
  Up/Down move, Enter select, Esc session-only
> [x] Session only   this launch
  [ ] Globally       remember launch preferences
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

Selecting global atomically writes the explicitly selected provider and/or
harness plus the effective harness's setup scope to `~/.clud/settings.json`.
The provider becomes `backend.default`; the harness becomes
`harness.default`. After
`clud --codex` is selected globally, later bare `clud` launches use Codex until
the user runs `clud --claude` and selects global. Selecting session-only stays
scoped to that one launch and does not rewrite either setting.

When an explicit provider flag differs from the stored `backend.default`, or an
explicit `--harness` differs from `harness.default`, clud shows the selector
even if the effective harness already has a stored global setup scope. Setup is
always selected and executed for that effective harness. This keeps temporary
provider or harness overrides from silently changing either default; only a
fresh `Globally` selection changes the explicitly selected dimensions.

A bare `clud` invocation (no provider/harness flag), non-interactive backend
launches, piped stdin, one-shot prompt launches (`-p` / `-m`), continuations,
and resumes do not prompt. They resolve the stored provider and harness
preferences when present and use session-only unless the effective harness's
stored setup scope says `global`.
`--dry-run` reads stored provider/harness preferences without initializing or
rewriting the settings document. It reports their effective values and sources
in JSON, while continuing to skip mutable global setup.
Self-contained maintenance commands exit before launch setup.

Session-only launches skip persistent setup. They must not create or modify
agent home setup files under `~/.claude`, `~/.codex`, `~/.agents`, or
`~/.clud` as part of harness setup. Bundled Python tools under
`~/.clud/tools/` are outside this launch-setup selector: normal foreground
startup, daemon startup, and `clud tool run` refresh clud-managed copies by
comparing the installed file with the embedded `BUNDLED_TOOLS` body and
replacing divergent managed copies.

## Global Actions

Global setup runs only the effective harness's registered actions:

| Effective harness | Action | Persistent paths |
|---|---|---|
| Claude | bundled skills | `~/.claude/skills/` |
| Claude | Claude drift skills | `~/.claude/skills/` |
| Codex | bundled skills | `~/.codex/skills/` gated by `~/.codex`; stale clud-managed `~/.agents/skills/` copies are purged |
| Codex | hook timeout normalization | `~/.codex/hooks.json` and `~/.clud/settings.lock` / `settings.json` |
| All | persisted global setup preference | `~/.clud/settings.lock` / `settings.json` |
| All | persisted model-provider preference | `~/.clud/settings.lock` / `settings.json` |
| All | persisted harness preference | `~/.clud/settings.lock` / `settings.json` |

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

Provider/harness precedence and the shared typed selector are documented in
[launch-targets.md](launch-targets.md).

## Adding an Action

Add a `HarnessSetupAction` implementation in `launch_setup.rs`, give it a
backend, and make `supports(SessionOnly)` false unless the action is proven not
to write persistent agent setup state. Tests should cover both session-only
no-write behavior and selected-backend global behavior.
