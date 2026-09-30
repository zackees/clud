# Agent deletion: `safe-rm`

`safe-rm` is the agent-facing deletion command. It accepts familiar `rm`
options: `-r`/`-R` for directories, `-d` for an empty directory, `-f` for
missing operands, and `-v` for removal messages. It moves accepted paths to
`~/.clud/trash/` by default. `--purge` permanently deletes after a GC audit
line; `--tracked` allows a tracked path that otherwise requires `git rm`;
`--dry-run` prints the plan. Options stop at the first path, and `--` allows
paths beginning with a hyphen. `clud safe-rm` and the `safe-rm` PATH alias run
the same implementation.

## Location policy

`CLUD_RM_ROOTS` is a path list supplied to the child session: the launch
checkout (or launch directory) and clud's session scratch directory. Outside a
session, `safe-rm` uses the current checkout or current directory. A checkout
root also covers its linked worktrees, and a linked worktree directory of an
allowed checkout may itself be removed, including one git already dropped from
`git worktree list` after a partly failed `git worktree remove` (its `.git`
file still names the allowed repo's `.git/worktrees/` metadata; #1573). Before a
move to the trash or a purge, directories in the tree are made writable, so
sealed read-only build outputs do not abort the removal halfway.

A separate clone of an allowed repo (a directory with its own `.git`
directory, outside the roots) may be removed only when nothing in it would be
lost (#1573). All of these must hold, checked in order, and a refusal names the
first that fails:

- its `origin` URL equals the `origin` of an allowed root after
  normalization: scheme, user, port, trailing `.git` or `/`, ssh
  (`git@host:owner/repo`) versus https, and the case of host/owner/repo are
  ignored (`origin mismatch`);
- `git status --porcelain` is empty, untracked files included and ignored
  files such as `target/` excluded (`dirty`);
- no local branch has commits that no `refs/remotes/*` reaches
  (`unpushed commits on <branch>`);
- `git stash list` is empty (`stash`).

A matching origin alone is not enough: a real checkout with unpushed work
would match. A clone that contains an allowed root, is HOME or an ancestor of
it, or whose `.git` is a file (a worktree or submodule) never qualifies. The
decision is `rm_tool_clone::verdict` over probed facts. The command refuses
a root itself,
anything outside these locations, HOME and its ancestors, filesystem roots,
`.git` components, and directory trees containing mounts. Symlinked parents
are resolved before authorization; a final symlink is moved as a link.

The main session and grind router may use the session roots. Grind integrators
are limited to their checkout; workers and reviewers to their recorded task
directories, or their checkout when no task directories were recorded. The
planner, prework, and lander profiles have no deletion permission. The rule IDs,
profile composition, redirect mappings, safe alias list, Claude deny rules, and
agent instruction text are defined in `deletion_rules.rs` and generated from
that table. New deletion behavior must be added there, with its frozen-ID and
consumer tests.

Rule directions describe the effect of adding a rule: redirects, refusals, and
root restrictions tighten policy; enabling `safe-rm` loosens it. Revoking a
tightening rule loosens policy. The planner, prework, and lander profiles are
the deliberate exception to the general "revocation loosens" rule: they revoke
the `delete/safe-rm` permission, which tightens policy to no deletion. This
preserves their specified behavior even though the composition operation is
still base union additions minus revocations.

## Trash and audit

One invocation creates one dated trash entry with `.clud-rm.json`, recording
the original and trashed paths. Same-filesystem paths are renamed; cross-device
paths are copied without following symlinks, then removed. The daemon's GC
registry and fallback sweep retain manifest-bearing entries for 72 hours.
Manifest-less `clud trash` quarantine entries keep their separate behavior.

Every call writes one JSONL record to `~/.clud/state/logs/rm/<date>.jsonl`
with role, cwd, roots, flags, per-path outcomes, and exit status. A batch may
trash valid paths while refusing others, returning nonzero. Nested paths within
a directory removed by the same call are skipped rather than removed twice.

## Agent hook and harness delivery

The command-scan hook rewrites direct, recognized agent-authored deletion to
`safe-rm`, preserving arguments and quoting. It refuses unsafe forms such as
nested shells, eval, privileged wrappers, and `find -delete`, with a suggested
safe command. Data in quotes, heredocs, prose, or read-only commands is not
treated as an executable deletion. A command consisting only of `safe-rm`
receives explicit allow so unattended runs do not stop on a permission prompt.
Per-profile location restrictions are checked before allowing a rewrite.

Claude receives generated deny patterns as a backstop when hooks are disabled,
plus the generated instruction paragraph. Codex receives the PreToolUse hook
and matching trust hash via launch `-c` arguments, plus developer instructions.
Existing user settings and hooks are preserved. Neither harness needs an
on-disk modification to the user's Codex configuration.

Human-authored scripts are different: their `rm` resolves to the session shim,
which enforces only the catastrophe floor and then hands off to the next real
`rm`. This is intentional, so ordinary installers and cleanup scripts can
delete outside the agent's roots. See [rm-protection.md](rm-protection.md).

This design supersedes the agent-deletion portions of DD-104; see the newer
decision in [DESIGN_DECISIONS.md](../DESIGN_DECISIONS.md).
