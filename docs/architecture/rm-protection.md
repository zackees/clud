# rm protection

Issue #1183 adds two complementary checks: the PreToolUse hook verifies the
session's effective PATH, and the argv[0] `rm` shim checks actual operands after
shell expansion. The source interpreter remains responsible for provenance,
redirection, indirect deletion and malformed payloads.

## Session installation and identity

Foreground `runner::child_env` and daemon `io_helpers::child_env_from` activate
the shim after assembling the child environment, including the client's PATH.
`shim_install::packaged_shim` locates `clud-shim` beside the running executable;
no environment variable chooses the trusted source. `install_rm_at` installs
only `rm` (`rm.exe` on Windows) in `~/.clud/state/rm-shim/`. This separate
directory avoids activating the unfinished Python relays in the older alias
installer. Every activation compares actual bytes and repairs replacements.
PATH prepending moves a later entry to the front and removes duplicates.

Both hook binaries use `block_bad_cmd_rm_identity`. Before allowing shell
commands they call the Rust `shim_resolve::which(name, path_env)` helper and
compare the selected executable's bytes with the packaged sibling. Missing,
unreadable, empty or replaced binaries deny with exit 2 and JSON. Relative or
empty PATH entries deny because their meaning depends on shell cwd. The packaged `tap` wrapper is transparent only after its own bytes match the
packaged sibling; its argv forwarding preserves the environment. Commands
that visibly bypass or change resolution also deny; command text is never
executed to investigate resolution.

This is the owner's PATH identity contract. It does not authenticate shell-local
hash tables, preexisting functions or aliases, nor prevent a hostile same-user
process replacing files after verification. Those require an execution-boundary
sandbox and are outside the enforceable guarantee of this PATH stub.

## Actual operands and execution

`rm_guard::decide` parses `--`, clustered `r/R/f/v` options and the supported
long equivalents, `--preserve-root` and `--one-file-system`. Unsupported options
and missing operands fail closed. It validates every operand before any removal.
The normalized deletion-base policy is shared with source analysis; existing
ancestors are canonicalized for nonexistent operands. Resolution errors,
protected roots, home roots and mount boundaries deny. Final symlink operands
are refused rather than changing unlink semantics into referent deletion. Execution is supported
only on Linux; other platforms fail closed.

`CLUD_RM_DRY_RUN=1` reports the decision without spawning. The real executor is
under `#[cfg(not(test))]`; unit-test execution can only report dry-run verdicts,
including when injected gate facts would otherwise authorize execution. No unit
test callback can invoke a removal implementation.

Real execution requires both a set environment-variable name containing uppercase
`CI` (its value is irrelevant) and Docker evidence from the running filesystem.
There is no enabling Docker override. Approved requests delegate normalized,
validated arguments to absolute `/bin/rm` through running-process, with
`--preserve-root=all` and `--one-file-system`; no PATH lookup or shell is used.
Ordinary removal outside CI + Docker is intentionally denied.

## Trusted Codex standalone updates

On Linux, `clud codex-update` is the explicit update route. The same route is
used when CLUD bootstraps a missing Codex backend. CLUD fetches the installer
from the fixed OpenAI release URL, rejects redirects, caps its size, and checks
the reviewed SHA-256 before running it. Its child environment contains only a
canonical HOME, noninteractive mode, and a system-tool PATH (including the
root-owned NixOS system profile when present). It retains the user's shell name
and, only if already present on the original PATH, the HOME-based `~/.local/bin` entry after system
tools. No other session PATH entry, installer-location override, or
deletion-gate variable is inherited.

This is a scoped installer operation, not an exception in `rm_guard`: callers
cannot supply shell text, a script path, or deletion operands to the command.
The usual shell pipeline to an installer still inherits the session shim and
still fails closed. When OpenAI changes the installer body, CLUD refuses the
new digest until the script is reviewed and the pin is updated. Neither the
foreground nor daemon child environment changes its ordinary removal policy.

## Retirement rationale

No source protection is retired. The only code moved out of the interpreter is
`unsafe_delete_base_reason` and its component-count helper, now owned by
`deletion_policy.rs` and called from both layers. This removes the need to
maintain a second copy of the policy without changing its corpus verdicts.
Variable provenance disappears during expansion; an executable cannot replace
the interpreter's proof of nonempty literal bases. Other programs and shell
redirections do not enter the rm shim. The PreToolUse identity backstop remains
mandatory even when source analysis finds a command benign.

## Verification

Rust tests cover PATH lookup/order, byte repair, PATH movement, argument parsing,
canonical ancestors and the complete gate truth table. Python process tests use
only dry-run for local rm invocations. See
[the Docker harness](../../ci/docker/rm_protection/README.md) for inert JSON corpus
checks and the separate disposable-file execution checks.
