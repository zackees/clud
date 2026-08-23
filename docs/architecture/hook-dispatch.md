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

## Which event an invocation serves, and who dispatches

A bare `clud-cmd-scan` means `PreToolUse`. That is what every already-installed
hook line means, and those lines keep working untouched. Other events are named
explicitly: `clud-cmd-scan --event Stop`.

Two things can therefore invoke the dispatcher for PreToolUse — the bare line a
user installed, and the `--event PreToolUse` line clud compiles — and a declared
hook must run exactly once. The tie is broken by `CLUD_HOOK_DISPATCH`, which
clud sets in the session env whenever it registered compiled lines:

| invocation | Tier A | Tier B |
| --- | --- | --- |
| bare, no marker | yes | yes — nothing else can |
| bare, marker set | yes | no — the compiled line owns it |
| `--event <E>` | — | yes |

Sessions clud did not launch never see the marker and keep the old behavior.
This is why the invocation carries an `explicit` flag rather than just an event
name: a bare call and a compiled `--event PreToolUse` call name the same event
but play different roles.

## Delivery: compiled into CLI args, not written to files

clud never writes hook lines into a user's config. It **compiles** its hook set
into each frontend's native surface and passes it at launch:

- **Claude** takes `--settings <file-or-json>`, an *additional* source that
  merges with the settings files rather than replacing them, with hook entries
  concatenating across levels. `foreground_runtime.rs` already composes such a
  document into a session-lifetime tempfile and injects `--settings`, and
  already merges into a user-supplied `--settings` so neither shadows the
  other.
- **Codex has no argument surface for hooks at all.** Its `-c key=value`
  overrides values that would otherwise come from `config.toml`, and codex
  hooks live in a separate `hooks.json`; no flag points at an alternate one,
  and `CODEX_HOME` would relocate auth and config along with it. So codex keeps
  the PreToolUse coverage its already-installed `clud-cmd-scan` line gives it —
  which runs declared hooks too — and gets nothing for other events, matching
  codex's own apparent single-event support. clud does **not** write
  `~/.codex/hooks.json` to close the gap.

This removes a whole hazard class that writing files would carry: idempotence,
read-modify-write lost updates, two writers fighting over one file (the #847
failure mode), per-repo gitignore assumptions, and stale state left behind by a
killed session.

`--setting-sources` can also *subtract* a source, which would let clud absorb a
repo's existing `.claude/settings.json` hooks and fix repos with no migration
at all. Deliberately not done yet: excluding a source drops everything it
contributes, so a bug there costs the user `permissions`, not just hooks. See
#977.

## Which repo's hooks fire

A session can touch files in more than one repo, and which repo's hooks apply
is decided by what the containing repo *is* to the session, not by path
geometry:

| kind | how it is registered | parent hooks fire there? |
| --- | --- | --- |
| `parent` | the session root | yes |
| `extern` | immediate children of `.extern-repos/` | **never** |
| `child` | declared in `.clud/settings.json` | yes |
| unregistered | — | no |

`extern` roots are temporary, foreign visits: the parent's guards have no
business running against a repo it does not own and will not keep, and firing
them there is the #841 ENOENT wedge. A declared `child` is the opposite — part
of the parent's world, so the parent's guards apply to it.

Nested git repos are **not** auto-detected as children. Declaration is the
consent that makes the child tier's no-prompt trust sound, and that reasoning
collapses if nothing was declared.

### Containment comes from what the call names

Never from the payload cwd alone: a subagent editing
`.extern-repos/<sub>/src/lib.rs` usually still has the session cwd at the
parent root, so keying on cwd would answer "parent" for a file that is plainly
not the parent's. The resolution order is:

1. paths the tool names (`file_path`, `notebook_path`, `path`)
2. otherwise, for Bash, wherever the command would `cd` to — `cd
   .extern-repos/dep && make` does its work in the sub-repo, and cwd is only
   where it started
3. otherwise, cwd

A call that spans repos still gets the parent's guards for the parent's own
files: any touched path the parent owns is enough.

Roots clud resolves at launch that a hook cannot rediscover — `--add-dir`
targets and `permissions.additionalDirectories`, neither of which appears in a
hook payload — cross the process boundary in `CLUD_HOOK_ROOTS`, as JSON,
because a path-separated list is ambiguous on Windows where paths contain
`:`.

## Where the code is

| file | role |
| --- | --- |
| `clud_hooks.rs` | `.clud/hooks.json` schema, discovery, matcher semantics |
| `clud_hook_roots.rs` | the typed root registry, containment, and the firing rule |
| `clud_hooks_compile.rs` | compiling declarations into a frontend's native registration, and the merge that keeps it from displacing anything |
| `foreground_runtime.rs` | composing the launch-scoped `--settings` document and injecting it into argv |
| `clud_hooks_run.rs` | Tier-B execution: rooting, stdin payload, exit-code contract |
| `block_bad_cmd.rs` | the hook binary: event arg, Tier A, then Tier B |
| `block_bad_cmd_cd.rs` | `bash.block_cd` cwd pinning, and the hook-command scanner both it and `hook_health` use |
| `hook_health/inspect.rs` | warnings: broken `git rev-parse` prefix, double-declared hooks |

## Not yet built

Phases 3–5 of #967: the typed root registry (`extern` vs `child` repos) and
path-based containment, Tier-B trust for foreign checkouts, and the `"auto"`
relaxation of `bash.block_cd` once a repo's hooks are dispatcher-managed and
therefore cwd-immune.
