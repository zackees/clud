"""Committed hook configs must stay portable (#1334).

clud's launch rollout once rewrote the repo's tracked `.claude/settings.json`
and `.codex/hooks.json` to the absolute path of whichever clud build was
running, and #1320 committed `/home/.../target/debug/clud-cmd-scan` to
`main`. That path exists on no other machine, so every PreToolUse hook failed
open and the command guard silently stopped running.

This lint fails `bash lint` when a tracked hook config's `command` names an
absolute path to a `clud-*` binary, or anything under a `target/` build
directory. Hook commands resolve clud's helpers from PATH (`clud-cmd-scan`).
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

from ci.process import check_output

ROOT = Path(__file__).resolve().parents[1]

#: Hook config files, by their path suffix inside any directory.
HOOK_CONFIG_SUFFIXES = (
    ".claude/settings.json",
    ".claude/settings.local.json",
    ".codex/hooks.json",
    ".clud/hooks.json",
)

#: An absolute path (POSIX `/…`, `~/…`, or Windows `C:\…`) ending in a
#: `clud-*` program.
ABSOLUTE_CLUD_BINARY = re.compile(
    r"""(?:^|[\s'"&=(])(?:[A-Za-z]:[\\/]|/|~[\\/])[^'"\s;]*?[\\/]clud-[\w.-]+"""
)
#: Any path through a cargo `target/` directory.
TARGET_DIR = re.compile(r"""(?:^|[\\/\s'"])target[\\/](?:debug|release|[\w.-]+[\\/])""")

REASON = (
    "a committed hook config must not pin a machine-local clud binary or a "
    "build output under target/; use the bare `clud-cmd-scan` (resolved from "
    "PATH) so the hook works on every checkout."
)


def tracked_files() -> list[str]:
    out = check_output(["git", "ls-files", "-z"], cwd=ROOT)
    return [p for p in str(out).split("\0") if p]


def is_hook_config(rel: str) -> bool:
    rel = rel.replace("\\", "/")
    return any(rel == suffix or rel.endswith("/" + suffix) for suffix in HOOK_CONFIG_SUFFIXES)


def _commands(value: object) -> list[str]:
    found: list[str] = []
    if isinstance(value, dict):
        command = value.get("command")
        if isinstance(command, str):
            found.append(command)
        for child in value.values():
            found.extend(_commands(child))
    elif isinstance(value, list):
        for child in value:
            found.extend(_commands(child))
    return found


def is_banned_command(command: str) -> bool:
    return bool(ABSOLUTE_CLUD_BINARY.search(command) or TARGET_DIR.search(command))


def scan(text: str) -> list[str]:
    """Banned hook commands in one config's text.

    An unparsable file is scanned as raw text, so a syntax error cannot hide
    a pinned path.
    """
    try:
        commands = _commands(json.loads(text))
    except ValueError:
        commands = text.splitlines()
    return [command for command in commands if is_banned_command(command)]


def main() -> int:
    total = 0
    for rel in tracked_files():
        if not is_hook_config(rel):
            continue
        try:
            text = (ROOT / rel).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        for command in scan(text):
            print(f"{rel}: BANNED hook command `{command}` — {REASON}", file=sys.stderr)
            total += 1
    if total:
        print(f"\n{total} non-portable hook command(s) found.", file=sys.stderr)
        return 1
    print("No non-portable hook commands found.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
