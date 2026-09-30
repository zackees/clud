# In-session clone and worktree redirect (`git` / `gh` aliases, #1486)

Inside a clud session, `git clone` or `git worktree add` would put a checkout
wherever the agent chose, outside every GC root, and nothing would reclaim
it. The session's `git` and `gh` aliases refuse the argv forms that create a
new checkout and point the agent at two helpers that write only into
`~/.clud/tmp-wt`, the root the daemon GC owns
([gc-and-registry.md → worktree root](gc-and-registry.md#the-worktree-root-cludtmp-wt-1485)).
This is worktree hygiene, not a jail: an absolute `/usr/bin/git` skips the
PATH alias, as it does for `rm` (#1305).

The alias mechanics (registry row, session ABI stamp, fail-open passthrough
outside a session) are owned by [shim-dispatch.md](shim-dispatch.md); the
`gh pr checks --watch` upgrade by [gh-watch-shim.md](gh-watch-shim.md).

## The names

`git`, `gh`, `safe-gh-clone` and `safe-gh-worktree` (plus `.exe` on
Windows) are argv\[0\] names of the one `clud` binary
([DD-121](../DESIGN_DECISIONS.md#dd-121-helper-executables-are-argv0-aliases-of-the-one-clud-binary)),
installed into the session alias directory `~/.clud/state/rm-shim` by
`shim_install::install_rm_at`. `git` and `gh` are `Passthrough` rows; the two
helpers are `Native` rows, so they also work outside a session.

## The policy

`crates/clud-bin/src/git_gh_policy.rs` is pure over argv. After skipping
git's global options (`-C <p>`, `-c k=v`, `--git-dir[=]`, `--work-tree[=]`,
`--no-pager`, `-P`, `--bare`, `--namespace[=]`, `--exec-path[=]`,
`--config-env`, `--attr-source`, and any other flag), it refuses:

| argv | redirected to |
| --- | --- |
| `git clone …` | `safe-gh-clone` |
| `git worktree add …` (other `worktree` subcommands pass) | `safe-gh-worktree` |
| `gh repo clone …` | `safe-gh-clone` |
| `gh repo fork … --clone` / `--clone=<true>` | run without `--clone`, then `safe-gh-clone` |
| `gh repo create … --clone` / `-c` | run without `--clone`, then `safe-gh-clone` |

Everything else, `git submodule` included, reaches the real binary with the
original argv bytes, stdio and exit status (`exec` on Unix). Git aliases
(`git config alias.cl clone`) are not expanded; they are the same bypass
class as an absolute path.

## The refusal message

The first line is a frozen contract constant (`REFUSED_GIT_CLONE`,
`REFUSED_GIT_WORKTREE_ADD`, `REFUSED_GH_REPO_*`); the exit status is
`REFUSAL_EXIT_CODE` (2) and the refused command never runs. The rest names
the helper, its invocation form, and a copyable example:

```
git worktree add is redirected inside a clud session
  use: safe-gh-worktree <repo-slug> <issue-number> [--path <reserved-dir>] [<git worktree add options>...]
  e.g. safe-gh-worktree clud 432 --path /home/u/.clud/tmp-wt/clud-wt-432 -b feat/432-x origin/main
  -> /home/u/.clud/tmp-wt/clud-wt-432  (reserved now; without --path a call takes the next free ordinal, e.g. clud-wt-432-2)
```

The printed path **exists** when the message is printed: `refuse` reserves it
through `alloc_wt_path` first
([DD-125](../DESIGN_DECISIONS.md#dd-125-a-clone-refusal-reserves-the-path-it-prints)).
The slug is the main checkout's directory name (walking up to `.git`, and
through a linked worktree's `gitdir:` to its main repo), or the last
component of the clone source; the worktree suffix is the first digit run of
the `-b` branch or the requested path, else that path's name. An unused
reservation is an empty directory the daemon reclaims as `reserved-unused`
after 24 h ([gc-and-registry.md](gc-and-registry.md#the-worktree-root-cludtmp-wt-1485)).

## The helpers

`crates/clud-bin/src/safe_gh.rs`:

- `safe-gh-worktree <repo-slug> <issue-number> [--path <dir>] [opts…]` runs
  `git worktree add <dest> <opts…>` in the current repo.
- `safe-gh-clone <owner/repo | url | path> [--path <dir>] [opts…]` runs
  `git clone <opts…> <src> <dest>` for a URL, scp address or local path, and
  `gh repo clone <owner/repo> <dest> [-- <opts…>]` otherwise.

`<dest>` is `--path` when given (it must be an existing, empty directory
directly inside the root: what a refusal reserved) or a fresh
`alloc_wt_path(slug, suffix)` (`clud-wt-432`, then `clud-wt-432-2`, …). On
success the absolute destination is the last line of stdout; on failure a
directory the call allocated itself is released with `remove_dir`.

## clud never blocks itself

The launching clud resolves the real `git` before the alias directory goes on
PATH (`shim_session::activate_rm`) and exports it as `CLUD_GIT_SHIM_TARGET`,
beside `CLUD_GH_SHIM_TARGET`. `shim_registry::real_program` returns that
target when it is absolute, exists and is outside every alias directory.
`subprocess::command_spec_for_subprocess`, the chokepoint for clud's own
spawns (GC probes and reclaim, `worktrees.rs`, grind, `pr_merge_watch`
lookups), rewrites a bare `git` / `gh` to it, and the helpers run it too. A
`gh repo clone` child gets the real `git`'s directory first on PATH, because
gh finds git by PATH lookup. Commands a Python hook or bundled tool runs by
bare name reach the alias, which passes every non-clone command through.

## Tests

- `git_gh_policy_tests.rs`: every refused and passed form, global-option
  skipping, slug/suffix derivation, the frozen strings, and a printed path
  that exists.
- `safe_gh.rs` and `shim_registry.rs` unit tests: `--path` validation,
  ordinals, and `real_program` never selecting an alias directory.
- `tests/test_git_shim.py`: pass-through against a recording fake, the
  refusals, the helpers' ordinals, outside-session passthrough and
  `git: command not found`, and the Windows `git.exe` alias.
