# Hook dispatch

How clud runs a repo's hooks itself, so they stop depending on where the agent
happens to be standing.

Design record: [#966](https://github.com/zackees/clud/issues/966) (spec),
[#967](https://github.com/zackees/clud/issues/967) (phased plan),
[#977](https://github.com/zackees/clud/issues/977) (settings compilation),
[#965](https://github.com/zackees/clud/issues/965) (the failure that started
it). Decisions: DD-047.

## The problem

A `cd` in a Bash tool call moves the **session** cwd, not just that command's.
Every later tool call inherits it. Hooks are conventionally written as
repo-relative script paths — `uv run python ci/hooks/check-on-stop.py` — so
once the cwd drifts, they resolve against the wrong directory and fail. When
the failing hook is a blocking one, nothing can run: the session wedges until a
human intervenes.

Three things follow from that, and they are what this subsystem exists to fix:

1. Hooks must not depend on cwd.
2. A nested repo's own hooks never load at all — the harness reads hooks only
   from the session root.
3. The parent's hooks *do* keep firing inside a nested checkout, against files
   they know nothing about.

Upstream will not fix this: the cwd contract is documented as following the
agent, and the tracker's own reports disagree about whether cwd drifts or
silently resets (anthropics/claude-code#83636, #76708, #84685; the exact class
in #50960 and #42282 was closed NOT_PLANNED).

## Two layers

**Tier A — clud policy.** Built-in rules plus `bad_commands` / `bad_pipelines`,
evaluated in-process on every tool call. No trust gate: it is clud's own code,
and it self-roots per path via `discover_effective_clud_config`. This is what
`clud-cmd-scan` has always done.

**Tier B — the repo's declared hooks.** Whatever `.clud/hooks.json` declares,
run by clud rather than by the harness. This is the part that gets the rooting
contract.

Tier A runs first. A project guard runs last so its message is not buried under
one clud would have produced anyway.

## Declaring hooks

`<repo>/.clud/hooks.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      { "matcher": "Bash", "command": "uv run python ci/hooks/check_cmd.py" }
    ],
    "Stop": [
      { "command": "uv run python ci/hooks/check_on_stop.py", "timeout": 120 }
    ]
  }
}
```

- `command` is a **shell command line**, the same shape both frontends accept.
- `matcher` is a regex over the tool name, anchored so `Edit` cannot catch
  `MultiEdit`. Absent or `"*"` matches everything. A pattern that will not
  compile falls back to exact equality — a declaration clud cannot understand
  should under-match, never over-match.
- `timeout` is seconds, defaulting to 60.
- Event names are free-form strings, so a declaration for an event clud has not
  learned to fire sits inert rather than failing the file.

Parsing is lenient in the same way `repo_clud_config` is: one malformed entry
is skipped with a warning rather than taking the file's other hooks down with
it.

**Migrating means moving, not copying.** A command left in
`.claude/settings.json` *and* declared here runs twice, and the copy the
harness fires is the unrooted one — the exact failure the declaration was
supposed to fix. `hook_health` warns when it sees the same command in both.

## The execution contract

Every declared hook runs with:

- **cwd = the declaring repo's root**, whatever the session cwd is.
- **`CLUD_PROJECT_DIR` = that same root**, uniform across frontends.
  `$CLAUDE_PROJECT_DIR` is Claude-only and names the *session* root, so it
  cannot root a sub-repo's hook.
- **the harness's payload on stdin**, forwarded byte-for-byte, with the pipe
  closed afterwards so a hook blocking in `json.load(sys.stdin)` gets its EOF.

Exit codes mirror the harness's, because these are the same scripts users
already wrote for it:

| child exit | meaning |
| --- | --- |
| 0 | continue to the next hook |
| 2 | block: stop, and relay the child's own output as the reason |
| other | the hook is broken — warn, continue |
| timeout | warn, continue |

The last two rows fail **open** on purpose. A guard that cannot run is a bug in
the guard; turning it into a wall in front of every tool call is how a session
wedges, which is the outcome this whole subsystem exists to prevent. A blocking
hook's stdout is relayed verbatim rather than re-wrapped, since it may be
speaking the harness's own JSON protocol.

## Which event an invocation serves

A bare `clud-cmd-scan` means `PreToolUse`. That is what every already-installed
hook line means, and those lines keep working untouched. Other events are named
explicitly: `clud-cmd-scan --event Stop`.

## Delivery: compiled into CLI args, not written to files

clud never writes hook lines into a user's config. It **compiles** its hook set
into each frontend's native surface and passes it at launch:

- **Claude** takes `--settings <file-or-json>`, an *additional* source that
  merges with the settings files rather than replacing them, with hook entries
  concatenating across levels. `foreground_runtime.rs` already composes such a
  document into a session-lifetime tempfile and injects `--settings`, and
  already merges into a user-supplied `--settings` so neither shadows the
  other.
- **Codex** takes repeated `-c key=value` overrides, compiled in
  `command/builder.rs`. Codex appears to support only `PreToolUse`, so hooks
  for other events have no codex target.

This removes a whole hazard class that writing files would carry: idempotence,
read-modify-write lost updates, two writers fighting over one file (the #847
failure mode), per-repo gitignore assumptions, and stale state left behind by a
killed session.

`--setting-sources` can also *subtract* a source, which would let clud absorb a
repo's existing `.claude/settings.json` hooks and fix repos with no migration
at all. Deliberately not done yet: excluding a source drops everything it
contributes, so a bug there costs the user `permissions`, not just hooks. See
#977.

## Where the code is

| file | role |
| --- | --- |
| `clud_hooks.rs` | `.clud/hooks.json` schema, discovery, matcher semantics |
| `clud_hooks_run.rs` | Tier-B execution: rooting, stdin payload, exit-code contract |
| `block_bad_cmd.rs` | the hook binary: event arg, Tier A, then Tier B |
| `block_bad_cmd_cd.rs` | `bash.block_cd` cwd pinning, and the hook-command scanner both it and `hook_health` use |
| `hook_health/inspect.rs` | warnings: broken `git rev-parse` prefix, double-declared hooks |

## Not yet built

Phases 3–5 of #967: the typed root registry (`extern` vs `child` repos) and
path-based containment, Tier-B trust for foreign checkouts, and the `"auto"`
relaxation of `bash.block_cd` once a repo's hooks are dispatcher-managed and
therefore cwd-immune.
