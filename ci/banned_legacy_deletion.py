"""Reject retired deletion aliases and copied Claude deny rules (#1461).

The rule table generates active deletion policy. Historical decision records
and changelogs may retain old names, but code, tests, skills and live docs may
not. A purge-list entry may carry ``legacy-deletion-purge: allow`` on its line.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

from ci.process import check_output

ROOT = Path(__file__).resolve().parents[1]
PATTERN = re.compile(r"rm-(?:file|dir)|Bash\(rm\b")
PURGE_MARKER = "legacy-deletion-purge: allow"
SKIP_SUFFIXES = (".lock", ".png", ".jpg", ".gif", ".ico", ".wasm", ".bin", ".b64")


def scan(contents: str) -> list[tuple[int, str]]:
    return [
        (number, line.strip())
        for number, line in enumerate(contents.splitlines(), start=1)
        if PATTERN.search(line) and PURGE_MARKER not in line
    ]


def is_scanned(relative: str) -> bool:
    path = Path(relative)
    return (
        not relative.startswith("vendor/")
        and not relative.endswith(SKIP_SUFFIXES)
        and path.name not in {"DESIGN_DECISIONS.md", "CHANGELOG.md"}
    )


def main() -> int:
    paths = str(
        check_output(
            ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
            cwd=ROOT,
        )
    ).split("\0")
    count = 0
    for relative in sorted(set(paths)):
        if not relative or not is_scanned(relative):
            continue
        try:
            contents = (ROOT / relative).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        for number, line in scan(contents):
            print(f"{relative}:{number}: retired deletion policy: {line}", file=sys.stderr)
            count += 1
    if count:
        print(f"{count} retired deletion-policy reference(s) found.", file=sys.stderr)
        return 1
    print("No retired deletion-policy references found.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
