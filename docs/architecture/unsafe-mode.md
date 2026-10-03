# `clud --unsafe` policy

Design decision: [DD-155](../DESIGN_DECISIONS.md).

`--unsafe` is an explicit, per-launch choice to disable clud's own agent safety
rules. It is separate from `--safe`, which controls the backend's permission
prompts. A normal launch keeps every existing safeguard. An unsafe launch must
be visible in `--dry-run` and must not persist in user or repository settings.

## Scope

The switch applies to the launched session and its subagents, including a
daemon-hosted session. A nested `clud` launch chooses its own mode; an inherited
unsafe environment value cannot silently turn on unsafe mode in a new launch.
The session marker must come from the parsed launch flag and be propagated by
the shared child environment builder. Helpers must read only this marker, not
the user's ambient environment, to decide whether to bypass a guard.
The normal command scanner refuses agent-authored changes to that marker.

In unsafe mode, clud does not redirect `rm`, `rmdir`, or `unlink` to `safe-rm`,
refuse `find -delete`, inject the deletion instruction paragraph, or install
Claude deletion denies. The command-scan hook must not impose clud's built-in
command, deletion, root-override, role, repeat, recursion, or command-gate
restrictions. Clud's `rm` alias must hand off to the real `rm` without its
catastrophe checks or extra protective flags. Clud must not force shell
`nounset` or its command gate. An explicit `safe-rm` call remains a request for
its documented trash and location behavior; `--unsafe` never changes the
meaning of a command the caller chose explicitly.

Existing repository and user hooks are user policy, so their own decisions
still run. Session bookkeeping, GC tracking, authentication routing, telemetry,
model restrictions, and process cleanup continue because they are not safety
permission gates. The backend's permission mode remains controlled by `--safe`;
`--unsafe` does not override it. Reject using both flags together because their
names express contradictory permission choices.
Repository `bad_commands` and `bad_pipelines` settings are implemented by
clud's command-scan policy and are bypassed with it; independently declared
hooks still run.

## Verification

Test both Claude and Codex launch plans, foreground and daemon child
environments, and a nested launch. Verify that default behavior still rewrites
agent deletion and protects catastrophic `rm`; verify that unsafe mode lets a
real `rm` call and `find -delete` pass without clud rewriting, while an
explicit `safe-rm` retains its normal checks. Check that `--dry-run` reports
the mode and that the unsafe marker cannot be set by an inherited environment
value alone.
