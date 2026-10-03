# The child `rm` catastrophe floor

For the per-launch `--unsafe` bypass of this floor, see
[unsafe-mode.md](unsafe-mode.md).

The session puts a `clud` alias (hardlink, symlink or copy) named `rm` first on the child's PATH. This
shim is for commands run *by scripts*, including build tools and installers.
The floor applies only inside a valid clud session; outside one the alias is
the next real `rm`, unmodified ([shim-dispatch.md](shim-dispatch.md)). It
does not impose the agent's deletion policy: ordinary non-catastrophic operands
are handed to the next `rm` on PATH even outside `CLUD_RM_ROOTS`. Agent-authored
deletion is governed by the command-scan hook and `safe-rm`; see
[rm-tools.md](rm-tools.md).

The shim validates every operand before handing off any of them. It refuses
filesystem roots, their immediate children, the session HOME and its ancestors,
a whole-home glob, mount points, and a request to disable root preservation.
It resolves symlinked parents and trailing-slash operands before checking.
Mixed calls fail as a whole. Refusals exit 2 with a JSON decision and never
invoke the next `rm`. A missing target with `-f` is not a refusal; the real
`rm` decides its ordinary exit status and output.

Handoff is to the first `rm` after the shim directory on PATH, without shell
execution or recursion. GNU receives `--preserve-root=all` and
`--one-file-system` once; BSD receives `-x`; BusyBox uses the shim's own mount
checks. The shim propagates the real command's stdout, stderr, and exit status.
It appends one child-role audit record per call under
`~/.clud/state/logs/rm/`. No CI variable or Docker evidence is required.

The mount check compares device IDs and, on Linux, exact mount paths in
`/proc/self/mountinfo`. The latter catches same-device bind mounts. The
`--one-file-system` handoff also protects mounted descendants during recursive
removal. The shim checks for mounted descendants before handoff as well, so
same-device bind mounts and BusyBox's lack of GNU guard flags cannot bypass
the floor. A BusyBox applet symlink is invoked by its symlink name (`rm`),
not by the resolved multicall executable path. The safety guarantee is a
catastrophe floor, not a sandbox against a hostile same-user process or a
replacement of the next PATH executable.

Foreground and daemon launch paths install and repair the shim, place it first
on PATH, and expose the same `safe-rm` alias. The hook checks that this PATH
entry resolves to the packaged shim and refuses agent attempts to change PATH
or deletion-policy environment variables. Shell-local aliases, functions, and
hash tables remain outside that byte-identity check.

Tests include Rust guard and handoff tests, process tests using a recording
next-PATH stub for all refusal cases, and a bosn real bind mount on disposable
tmpfs. The latter verifies that deleting a mount operand or its parent cannot
remove the read-only mounted checkout content. All real deletion tests are
confined to temporary paths or disposable containers.
