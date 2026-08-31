"""Generate the `rm -rf $VAR/` payload corpus for the container check.

Writes two files of newline-delimited JSON, each line one complete
`PreToolUse` hook payload:

- ``deny.jsonl``  -- must be refused (exit 2, ``permissionDecision: deny``)
- ``allow.jsonl`` -- must NOT be refused, or the guard is an outage

**Nothing here runs a command.** Every string below is inert data destined for
the hook's stdin; the hook parses it and prints a verdict. The `rm -rf` text is
the input to a decision, never something handed to a shell. Keep it that way:
if you ever find yourself passing one of these strings to `os.system`,
`subprocess`, or a shell, you have misunderstood this file.

The corpus is generated rather than listed so it can grow along an axis instead
of one line at a time -- add a spelling, an operand, or a structure and the
cross product expands. The exhaustive version of this cross product lives in
`block_bad_cmd_rm_vars.rs`'s stress tests; this is the representative slice
that runs in a container.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

# Ways to spell the removal program.
RM_SPELLINGS = [
    "rm -rf",
    "rm -fr",
    "rm -r -f",
    "rm --recursive --force",
    "rm -rf --",
    "rmdir",
    "/bin/rm -rf",
    r"\rm -rf",
    "sudo rm -rf",
    "env rm -rf",
    "command rm -rf",
    "nice rm -rf",
    "busybox rm -rf",
    "timeout 5 rm -rf",
    "nohup rm -rf",
    '"rm" -rf',
    "$(which rm) -rf",
]

# Operands that expand to `/` when the variable is not a proven literal.
HAZARD_OPERANDS = [
    '"$V"/',
    "$V/",
    "${V}/",
    '"${V}"/',
    '"$V"/*',
    '"$V"//',
    '"$V"/.',
    '"$V"/..',
    '"$V"/build',
    "\"$V\"'/'",
    '"${V:-/}"/',
    '"${V#x}"/',
    '"${!V}"/',
    '"$1"/',
    '"$BUILD_DIR"/',
]

# Structures that wrap a statement. The removal must not be lost inside any.
STRUCTURES = [
    ("bare", "{c}"),
    ("trailing-stmt", "{c} ; echo done"),
    ("and-chain", "true && {c}"),
    ("pipe-target", "true | {c}"),
    ("background", "{c} &"),
    ("subshell", "({c})"),
    ("brace-group", "{{ {c}; }}"),
    ("for-body", "for i in 1 2; do {c}; done"),
    ("while-body", "while true; do {c}; done"),
    ("if-body", "if true; then {c}; fi"),
    ("case-body", "case x in x) {c};; esac"),
    ("function-body", "f() {{ {c}; }}; f"),
    ("eval", 'eval "{c}"'),
    ("bash-c", 'bash -c "{c}"'),
    ("nested-subshell", "( ( {c} ) )"),
]

# Removals reached through a program that takes paths from its input.
INDIRECT = [
    "echo {operand} | xargs rm -rf",
    "printf %s {operand} | xargs -0 rm -rf",
    "find {operand} -delete",
    "find {operand} -exec rm -rf {{}} +",
]

# Assignments that never reduce to one provable, safe literal.
UNPROVABLE_ASSIGNMENTS = [
    "V=/",
    'V=""',
    "V=/tmp/../..",
    "V=$(echo /tmp)",
    "V=${OTHER}",
    "V=/tmp; V=/",
    "read V",
    "if true; then V=/tmp; fi",
    "V=..",
]

# Must stay allowed. A guard that refuses everything is an outage, and these
# are deliberately near-misses.
BENIGN = [
    # Proven literals still rewrite, which is what keeps the guard usable.
    'V=/tmp/safe; rm -rf "$V"/',
    'V=/tmp/safe; rm -rf "$V"/*.txt',
    # Literal operands, no variable at all.
    "rm -rf /tmp/scratch",
    "rm -rf ./build",
    # A variable that never reaches a removal.
    'echo "$V"/',
    'ls "$V"/',
    'mkdir -p "$V"/nested',
    # `rm` present as a word, but not as the program being run.
    "git rm -r --cached foo",
    "docker run --rm ubuntu",
    "echo 'rm -rf /'",
    # Removal and variable live in unrelated pipelines.
    'rm -rf /tmp/x && echo "$V"/y',
    # No trailing slash: expands to an empty operand and deletes nothing.
    'rm -rf "$V"',
    # Single quotes are not an expansion.
    "rm -rf '$V'/",
    # Ordinary work that must never be disturbed. Deliberately avoids
    # commands the hook blocks for unrelated reasons (bare `cargo` is
    # governed by the repo's soldr policy, not by this guard).
    "git status --porcelain",
    "ls -la /tmp",
    "echo hello world",
]


def payload(command: str, cwd: str) -> str:
    """One hook payload as a single JSON line."""
    return json.dumps(
        {
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "cwd": cwd,
        }
    )


def deny_commands() -> list[str]:
    commands: list[str] = []
    for _, structure in STRUCTURES:
        for spelling in RM_SPELLINGS:
            for operand in HAZARD_OPERANDS:
                commands.append(structure.format(c=f"{spelling} {operand}"))
    for template in INDIRECT:
        for operand in HAZARD_OPERANDS:
            commands.append(template.format(operand=operand))
    for assignment in UNPROVABLE_ASSIGNMENTS:
        commands.append(f'{assignment}; rm -rf "$V"/')
    return commands


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", required=True, type=Path)
    parser.add_argument(
        "--cwd",
        default="/work",
        help="cwd reported in the payload; a path inside the container.",
    )
    args = parser.parse_args()
    args.out_dir.mkdir(parents=True, exist_ok=True)

    deny = deny_commands()
    (args.out_dir / "deny.jsonl").write_text(
        "".join(f"{payload(c, args.cwd)}\n" for c in deny), encoding="utf-8"
    )
    (args.out_dir / "allow.jsonl").write_text(
        "".join(f"{payload(c, args.cwd)}\n" for c in BENIGN), encoding="utf-8"
    )
    print(f"generated {len(deny)} deny cases and {len(BENIGN)} allow cases")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
