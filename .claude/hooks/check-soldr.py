#!/usr/bin/env python3
"""PreToolUse hook: route Rust toolchain calls through soldr.

Blocks bare ``cargo``/``rustc``/``rustfmt`` Bash invocations and tells the
agent to prefix them with ``soldr`` (so they resolve through the rustup-
managed toolchain via ``rustup which`` — see CLAUDE.md). If soldr itself is
not on PATH, the denial points the user at ``./install``.
"""

from __future__ import annotations

import json
import re
import shutil
import sys

GUARDED = ("cargo", "rustc", "rustfmt")
INSTALL_HINT = (
    "Install it with: ./install   "
    "(or ./install --global for a system-wide install)."
)


def first_command(cmd: str) -> str | None:
    """Return the first executable token in a shell command line.

    Skips leading env-var assignments (``FOO=bar BAR=baz cargo ...``) so we
    still see ``cargo`` as the head.
    """
    cmd = cmd.lstrip()
    while True:
        m = re.match(r"\s*[A-Za-z_][A-Za-z0-9_]*=\S*\s+", cmd)
        if not m:
            break
        cmd = cmd[m.end() :]
    m = re.match(r"\s*([^\s|;&<>()`]+)", cmd)
    return m.group(1) if m else None


def normalize(token: str) -> str:
    base = token.rsplit("/", 1)[-1].rsplit("\\", 1)[-1]
    return base.split(".", 1)[0]


def deny(reason: str) -> None:
    json.dump(
        {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        },
        sys.stdout,
    )


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except Exception:
        return 0

    tool_input = payload.get("tool_input") or {}
    command = tool_input.get("command")
    if not isinstance(command, str):
        return 0

    head = first_command(command)
    if head is None:
        return 0
    base = normalize(head)

    soldr_present = shutil.which("soldr") is not None

    if base == "soldr":
        if not soldr_present:
            deny(
                "soldr is not installed but the command requires it.\n"
                + INSTALL_HINT
            )
        return 0

    if base not in GUARDED:
        return 0

    if not soldr_present:
        deny(
            f"`{base}` is blocked: this repo requires Rust toolchain calls to "
            "go through soldr (https://github.com/zackees/soldr), but soldr "
            "is not on PATH.\n" + INSTALL_HINT
        )
        return 0

    deny(
        f"`{base}` is blocked: prefix Rust toolchain calls with `soldr` so "
        "they resolve through the rustup-managed toolchain. "
        f"Re-run as: soldr {base} ... (preserves remaining args)."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
