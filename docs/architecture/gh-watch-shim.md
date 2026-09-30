# Session `gh` watch shim

The child environment installs `gh` beside the active `rm` and `safe-rm`
aliases in `~/.clud/state/rm-shim`. `shim_session::activate_rm` resolves the
real executable from the incoming PATH *before* prepending that directory,
then exports its absolute path as `CLUD_GH_SHIM_TARGET`. Foreground and daemon
launches share this path through `runner::apply_child_env_policy`.

`clud` dispatches (as `clud-shim`) by invoked filename through its single dispatch path;
without a valid session the alias is the next real `gh` on PATH (see
[shim-dispatch.md](shim-dispatch.md)). In a session, for ordinary commands it
runs the real executable as a child with the original arguments and inherited
stdio, and returns its exit status. Signals are handled shell-style so the
wait status is unchanged ([DD-131](../DESIGN_DECISIONS.md#dd-131-the-git--gh-telemetry-shim-waits-for-the-child-instead-of-execing)). A PR-check watch is
instead parsed against a deliberately narrow flag contract. Numeric PRs go
directly to the bundled watcher; omitted, branch, and URL selectors are
resolved by the real executable's `pr view` command. The watcher runs through
the pinned `CLUD_EXE tool run github/pr_merge_watch.py` path; without a
valid `CLUD_EXE` the watch runs on the real executable unchanged. Unsupported
flags fail explicitly so the shim cannot silently change a future CLI's
output or wait contract.

The command guard permits the canonical PR-check watch only when
`CLUD_GH_SHIM_ACTIVE=1`. It still denies run-id watches and polling loops.
If installation or target resolution fails, that marker is absent/zero and
the old guard denial remains; the alias itself still relays to the real `gh`.
Every in-session `gh` call, relayed or watched, is run as a child and
recorded; see [git-gh-telemetry-shim.md](git-gh-telemetry-shim.md). The `git.pr_wait_fail_fast` setting gates
the guard; disabling it leaves the alias as an ordinary relay for commands
that are not intercepted.
