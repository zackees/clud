# rm-file / rm-dir: clud-controlled deletion

Issue [#1340](https://github.com/zackees/clud/issues/1340), under meta
[#1436](https://github.com/zackees/clud/issues/1436). An agent deletes through
two clud commands, `rm-file` and `rm-dir`. Its own `rm` is redirected to them,
and scripts it runs may use `rm` inside the session's roots. The layers that
still check the child `rm` shim's identity are in
[rm-protection.md](rm-protection.md). The rationale is in
[DD-104](../DESIGN_DECISIONS.md#dd-104-agents-delete-through-rm-file--rm-dir-which-trash-by-default-and-agent-typed-rm-is-redirected).

## The commands

| Command | Removes | Refuses |
|---|---|---|
| `rm-file <path…>` | files; a symlink is removed as a link, never its target | a directory (hint: use rm-dir) |
| `rm-dir <path…>` | directories, recursively | a file or symlink (hint: use rm-file) |

The only flags are `--purge` (delete instead of trash), `--tracked` (allow a
git-tracked path, or a directory holding one; otherwise use `git rm`),
`--dry-run`, and `--` before a path that starts with `-`.

The two commands are argv\[0\] aliases of `clud-shim`
(`bin/clud_shim.rs`) and are also available as `clud rm-file` / `clud rm-dir`,
which run the same code (`rm_tool::run`). Session activation
(`shim_session::activate_rm`) installs `rm`, `rm-file` and `rm-dir` as byte
copies of `clud-shim` in `~/.clud/state/rm-shim/`. That directory is first on
the child PATH, with its copies repaired on every launch.

## Roots

`CLUD_RM_ROOTS` is a path list (`std::env::split_paths` syntax). clud sets it
in every session child's environment (`runner::apply_child_env_policy`):

- the git checkout of the launch directory, or the directory itself outside a
  repo. The launch directory is the base environment's `PWD`, which on the
  daemon path is the client's cwd;
- clud's session temp directory, `~/.clud/tmp`, which holds every agent
  scratchpad.

When `CLUD_RM_ROOTS` is unset, for example when a person runs `rm-file`
outside a clud session, the root is the current git checkout, or the cwd
outside a repo. `$HOME` and its ancestors are never roots, even for a
session launched there. A root that is itself a git checkout also covers
that repository's linked worktrees (`git worktree list`). This is checked
only when a path falls outside the plain roots, so a `/grind` integrator can
clean its sibling `<repo>-wt-<id>` worktree. An agent may not set
`CLUD_RM_ROOTS` or `CLUD_RM_ROLE` itself: the rm identity check refuses an
assignment or export of either, as it does for `PATH`.

Every path is resolved (`rm_tool::resolve`) by canonicalizing its parent and
joining the final name unfollowed, so a symlinked directory on the way must
itself land inside a root. A path is always refused when it:

- is a filesystem root, or ends in `.` or `..` as typed, or is a root itself;
- is `$HOME` or one of its ancestors;
- lies outside every root, including through a symlinked parent;
- has a `.git` component (any case);
- is a directory containing a mount point (Linux).

## Trash, purge and audit

- **Trash.** One call is one entry, `~/.clud/trash/<utc>-<hex>-<first name>/`.
  Each path keeps its location relative to its root, under the root's name
  (`…/repo/build/obj`). The entry holds `.clud-rm.json`, with the command, cwd,
  session, role, and every `origin → trashed` pair, which is enough to restore
  by hand. A move is a rename on the same filesystem. Across filesystems it is
  a copy (symlinks stay links) followed by a delete.
- **Reaping.** The entry is registered with the daemon's GC registry as a
  `trash` row, best effort. The daemon keeps a manifest-bearing entry for 72
  hours (`rm_tool::TRASH_KEEP`, the same window as `~/.clud/tmp`) before
  reaping it. It also sweeps expired entries that never got registered
  (`reap_unregistered_rm_trash`). `clud trash` quarantine entries have no
  manifest and are still removed as soon as they can be.
- **Purge.** `--purge` deletes for real, after a `gc-audit.jsonl` line
  (`rm-tool.purge`).
- **Audit.** Every call appends one JSONL record to
  `~/.clud/state/logs/rm/<YYYY-MM-DD>.jsonl`. The record has the time, command,
  session id, role, cwd, roots, flags, exit code, and each path's action
  (`trashed`, `purged`, `refused`, `skipped`, `would-*`) with its reason and
  trash path. The role is `CLUD_RM_ROLE` when set; otherwise it is `agent`
  inside a session and `user` outside one.

**Batches.** `find … -exec rm-file {} +` and `xargs rm-file` pass many paths
per call. Every path is checked. A refused path is reported and skipped, the
others still go, and the exit code is 1. A path inside a directory the same
call removes is skipped, whichever order they come in (`find -depth` lists
children first). Options come before the first path; after it everything is a
path, so a file named `--purge` cannot switch the call to purge.

## The hook redirect

`block_bad_cmd_rm_redirect` runs in the PreToolUse hook for POSIX shell tools.
It runs after the `/grind` role caps and before the rm identity and rm-variable
checks. It refuses what the agent typed and names the replacement. The
replacement keeps the agent's own quoting and is suggested, not applied, so
the agent learns the command:

| Agent types (refused) | The message says to run |
|---|---|
| `rm -rf build` | `rm-dir build` |
| `rm a b`, `unlink a` | `rm-file a b`, `rm-file a` |
| `rmdir d` | `rm-dir d` |
| `find X -name '*.o' -delete` | `find X -name '*.o' -type f -exec rm-file {} +` |
| `find X -type d -name c -delete` | `find X -type d -name c -prune -exec rm-dir {} +` |
| `find X … -exec rm -rf {} +` | `find X … -prune -exec rm-dir {} +` |
| `… \| xargs -0 rm` | `xargs -0 rm-file` |

The redirect also sees through runner prefixes and their options (`sudo -u
root`, `env`, `command`, `tap`, `nice -n 10`, `timeout -s KILL 5`, …),
active command substitutions, and nested `bash -c` scripts. Text in single
quotes and heredoc bodies is data: `gh pr create --body 'Replaces `rm -rf
build`'` and a commit message passed through `$(cat <<'EOF' … EOF)` are not
redirected.
`git rm`, `git clean`, and inline `python -c` / `node -e` deletions are
deliberately not redirected.

**No prompt.** A command made only of `rm-file` / `rm-dir`, run directly
(the bare alias or its path in `~/.clud/state/rm-shim/`), through a bare
`clud` or `$CLUD_EXE`, from `find … -exec`, or after `xargs`, gets an
explicit `allow` from the hook (`rm_tool_only`). An environment prefix, a
wrapper, a substitution, a redirection or a background `&` disqualifies it.
So
Claude Code runs it without a permission prompt, even with prompts on. The
tools enforce the roots themselves, so a prompt would add nothing, and an
unattended run is never stopped on one (#963).

## Scripts: the child `rm` shim

A script's `rm` (`./test`, `make`, `cargo`) reaches the PATH `rm` shim. It is
allowed when `CLUD_RM_ROOTS` is set and every operand passes the same
`rm_tool::resolve` checks (`rm_guard::decide_in_roots`). The shim then deletes
in process on every platform (`-r` for directories, `-f` for missing
operands, `-v` to report), because scripts expect a real delete, and writes
an audit record with role `child`. Anything outside the roots, `/`, or `$HOME`
is still refused. The old CI-in-Docker gate remains for CI jobs without
roots. A script's `find -delete` never runs a program, so no shim can see it:
that gap is accepted.

## `/grind` per-role roots

Grind subagents share the session, so they all see the same
`CLUD_RM_ROOTS`. The hook narrows the roots per role
(`block_bad_cmd_grind_caps::rm_tool_verdict`), keyed on the payload's
`agent_type`:

| Role | May delete (`rm-file` / `rm-dir`) |
|---|---|
| `grind-worker`, `grind-reviewer` | only inside the directories of the files the planner recorded for its checkout (`clud grind-facts task`, stored in the session's run facts, [#1337](grind.md#router-questions)), or its whole checkout when none were recorded. Literal paths only: no `$`, backticks, `~`, globs, braces, backslashes or `find -exec` |
| `grind-integrator` | anywhere inside its checkout, including globs and `find -exec rm-file`; no `$`, backticks, `~`, braces or backslashes, which bash would expand to another path |
| the `/grind` router (main session) | the session roots |
| planner, prework, lander | nothing (their allowlists) |

## Identity check changes (#1305)

Agent-typed `rm` is now always redirected, so the rm identity check guards
what remains: the child shim's resolution. Its pre-checks no longer refuse a
backtick or `$(…)` they cannot parse unless the command also runs rm or a
nested shell, or a backtick forms a program name. `${…}` parameter expansions count as part of their word, and
`$(…)` bodies, already checked for a removal, are masked before the
statement analysis. See [rm-protection.md](rm-protection.md).

## Tests

- **Rust unit tests:**
  - `rm_tool_tests.rs`: argv\[0\] names, flags, roots from the env or the checkout, refusals, symlink escapes, trash layout and manifest, purge and dry run, batches, `--tracked` on a git fixture, audit fields, trash expiry.
  - `rm_guard`: the in-roots gate.
  - `block_bad_cmd_rm_redirect`: the replacement table and `rm_tool_only`.
  - `block_bad_cmd_grind_caps::deletion_roots_follow_the_role`.
  - `grind_facts`: concurrent task recording.
  - `gc_service` reaping.
  - `runner`: `CLUD_RM_ROOTS`.
  - `shim_install`: the three aliases.
- **Python process tests:** `tests/test_rm_shim.py` covers the child shim deleting inside the roots without CI or Docker, refusals outside them, the aliases trashing, and the hook's redirect and identity cases. `tests/test_hook_stdin.py` covers the redirect message and the no-prompt allow.
- **Real Claude Code** (`tests/harness/test_rm.py`, [testing-tiers.md](testing-tiers.md)):
  - `rm-dir` / `rm-file` run with permission prompts on and in bypass mode, with no denial, and land in `~/.clud/trash`;
  - an agent's `rm -rf build` is redirected, and its next step `rm-dir build` succeeds;
  - `find … -exec rm-file {} +` runs without a prompt;
  - a path outside the roots is refused;
  - a grind worker is refused outside its task's directories, while the integrator's `rm-dir` inside its checkout succeeds.
