# Checkout claims (#1342)

Each foreground `clud` launch announces its canonical Git worktree and common
Git directory to the daemon. The daemon keeps that presence only while its
TCP connection is alive. A claim is an exclusive flag on a presence, keyed by
the worktree path; sibling worktrees may each be claimed. Dropping the
connection, including after a killed client, removes its claim immediately.

`clud grind`, `clud do`, and `clud fix` acquire a claim before launching their
agent. The `/grind` and `/clud-fix-quick` skills run `clud claim acquire` when
invoked inside an ordinary session. A second claimant gets the holder's
session ID, tool and run, then must use another worktree or wait. An ordinary
launch only warns when others are present. `clud list` shows live sessions and
claims. `clud claim who` filters to the current worktree. Skills call
`clud claim release-own` at completion; `clud claim release <checkout>` asks
for confirmation before clearing another live claim.

The foreground client records claim intent in its daemon state directory,
reconnects after a daemon restart and restores its claim. It removes the marker
on normal exit; a killed process can leave a marker, but no live connection to
restore it. The daemon delays new claims for two seconds after startup so
existing clients can restore first. If a restoring client finds another
session obtained its claim, clud interrupts its agent. The native PreToolUse
hook follows shell `cd`/`pushd` segments and the checkout selected by Git's
`-C`, `--work-tree`, or standard `.git` directory path options
before `git switch`, `checkout`, `commit`, `reset`, `stash`,
`rebase`, or `merge`; it denies mutation when a different session holds the
worktree. `CLUD_CHECKOUT_CLAIM_OVERRIDE=1` bypasses that hook check. If the
daemon cannot be reached, claim requiring commands fail and the hook warns
but permits the command.

The checkout identity variable is `CLUD_CHECKOUT_SESSION_ID`, distinct from
the session ID used by the trash and session history features. Claim RPCs use
the daemon's existing loopback TCP listener with a tagged JSON request; a
`present` connection remains open until the foreground client exits.
