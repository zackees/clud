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

### System temp directories (#1622)

Files and directories **strictly under** a system temp directory are always
deletable, in and out of a session: `/tmp`, `/var/tmp`, `/dev/shm` (#1659)
and `$TMPDIR` on Unix (macOS's `/var/folders/.../T`), `%TEMP%` and `%TMP%` on
Windows. `/run/user/<uid>` is deliberately not one: it holds live session
sockets. Each is
resolved to its canonical form (Windows `\\?\` prefix stripped by
`path_norm::canonicalize_plain`) when the roots are built
(`rm_tool::system_temp_roots`). Every other check still applies:

- the temp directory itself (`safe-rm /tmp`, `/tmp/`, `/tmp/.`) is refused;
- the operand's parent is canonicalized before matching, so `/tmp/../etc/x`
  is judged as `/etc/x` and `/tmp/link/x` with `link -> /home/u` as
  `/home/u/x`; a final symlink is removed as a link, never followed;
- a temp directory that is a filesystem root, is HOME or an ancestor of it is
  dropped. On Unix one inside HOME (`TMPDIR=~/Documents`) is dropped too; on
  Windows only the profile's `AppData\Local` may hold it. The command-scan
  hook refuses a deletion command that assigns `TMPDIR`, `TEMP` or `TMP`,
  like `CLUD_RM_ROOTS`.

**Ownership.** `/tmp` is shared and world-writable. Its sticky bit already
stops unlinking another user's entry directly in `/tmp`, but not a file inside
a directory another user left writable, and trashing (a rename) would move it
out of their reach. So on Unix every existing entry from the temp directory
down to the operand must be owned by the effective uid, or the path is refused
(`... is not owned by you`). Windows has no equivalent check: `%TEMP%` is
per-user under the profile and protected by its ACL, so ownership adds nothing
there ([DD-128](../DESIGN_DECISIONS.md#dd-128-safe-rm-always-allows-entries-under-the-system-temp-dirs-owned-by-you-on-unix)). The refusal for a path outside every root lists the temp directories
with a `(temp)` suffix, states that `--purge` does not relax the check (it
picks trash versus delete, never where), and names the session temp dir as the
place for scratch data.

**RAM-backed temp roots (#1659).** A temp root on tmpfs or ramfs
(`rm_tool::is_ram_backed`, Linux `statfs`; `Roots::ram_roots`) has its entries
purged even without `--purge`, with the same GC audit line, and `--dry-run`
reports `would-purge`. The trash is on disk, so trashing would copy RAM-held
data there for 72 hours ([DD-134](../DESIGN_DECISIONS.md#dd-134-devshm-is-a-safe-rm-temp-root-and-ram-backed-temp-entries-are-purged-not-trashed)).

**Agent guidance.** Write scratch data under the session temp dir; use
`/dev/shm` only when the work must stay off disk (benchmarks), and clean it up
with `safe-rm -r`. Do not point the scratchpad at tmpfs through a symlink: its
canonical form would leave the roots. Scratch that must live elsewhere is made
with `safe-mktemp <path>` (Unix; see the creation ledger below), so `safe-rm -r`
can remove it later. Any other path a session wrote outside the roots is
still refused; the agent leaves it and reports the exact path to the user. Do
not bypass `safe-rm` with another deletion route.

### Creation ledger

[DD-135](../DESIGN_DECISIONS.md#dd-135-a-creation-ledger-for-safe-rm-is-hybrid-daemon-held-and-written-only-by-clud-creating-the-path)
records the decision: a hybrid ledger (directories clud created are deletable
wholesale; single files clud created inside pre-existing directories are
deletable individually) held as session-keyed rows in the daemon's GC
registry, written only by a clud helper that performed the create itself, and
matched by device/inode at delete time. Sub-agents share the parent session's
id and environment, so their entries serve the parent.

Built: the store and the consult (slice 1, #1666) and the directory creator
`safe-mktemp` (slice 2, #1667). Still design: recording single files clud
creates inside pre-existing directories, and the human override (#1668).

- **Store.** The `created` rows live in `data.redb`'s `created_entries` table,
  keyed by `(session id, canonical path)`, each holding kind (`dir`/`file`),
  creator role, time, and on Unix device, inode and uid
  (`gc::CreatedEntry`). They survive daemon restarts and expire on the GC tick
  after the same 72h window as the session's temp directory and trash entries
  (`gc::session_tmp::STALE_THRESHOLD`). Only the registry worker touches them,
  through `GcOp::InsertCreated` (internal API, `daemon::gc_client_insert_created`)
  and `GcOp::QueryCreated`.
- **Consult.** For a canonical path outside every root and every temp root,
  `safe-rm` asks the daemon (`daemon::gc_client_query_created`, 2s timeout,
  never spawning one) and allows the path when it is a recorded file or is or
  lies under a recorded directory, that entry is still the same device/inode
  and not a symlink, and every existing entry from it down to the target is
  owned by the caller (the DD-128 rule). The audit record's `reason` names the
  ledger row. The decision is `rm_tool::ledger::verdict` over injected
  `LedgerFacts`.
- **Refusals.** An unreachable or old daemon, or no session id, keeps today's
  strict refusal plus `creation ledger unavailable (...)`; a path no row
  covers adds `not created by this session`; a swapped, replaced or foreign
  entry names what changed. A recorded directory swapped for a symlink is
  refused, and a path through such a symlink canonicalizes away from the row,
  so its target is never reached.
- **Writer: `safe-mktemp <path>`** (#1667,
  [DD-136](../DESIGN_DECISIONS.md#dd-136-safe-mktemp-is-the-only-ledger-writer-and-undoes-its-mkdir-when-the-insert-fails)).
  A multicall name of `clud` (`crate::safe_mktemp`), installed beside
  `safe-rm`, and the only caller of `gc_client_insert_created`; a source-scan
  test keeps it that way, and no CLI form records an arbitrary path. It makes
  exactly one directory with an exclusive `mkdir` (mode 0700): an existing
  path of any kind, including a symlink, fails with nothing created or
  recorded, and a missing parent fails (parents are never created, so never
  recorded). It opens the new directory `O_NOFOLLOW`, requires it to be an
  empty directory the caller owns whose handle identity still matches the
  path, records that device/inode with the session id and `CLUD_RM_ROLE`
  (default `agent`), and prints the canonical path. If the daemon insert
  fails, it removes the directory again (only while it is still that empty
  directory) and exits 1; with no session id it creates nothing and exits 2.
  The generated agent guidance (`deletion_rules.rs`) tells agents to make
  out-of-roots scratch with it.
- **Windows.** No file identity is available there without new unsafe code,
  so the device/inode check is Unix-only and the ledger refuses on doubt:
  Windows behaves as before, with the refusal saying the identity cannot be
  verified. `safe-mktemp` therefore fails there with exit 2 before creating
  anything, and the agent guidance omits it.

Grind profiles keep their narrower scope: `block_bad_cmd_grind_caps` still
limits integrators to their checkout and workers/reviewers to their task
directories, so the temp allowance widens only the main session and
user-invoked `safe-rm`.

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
a root itself, a system temp directory itself,
anything outside these locations (a temp entry another user owns included), HOME and its ancestors, filesystem roots,
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
