#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
# managed-by: clud
"""extract_zip.py — extract a `.zip` archive into a destination directory.

Internal helper for `clud optimize`'s soldr-binary install path. Replaces
the previous `powershell -Command Expand-Archive` invocation so the
zip-extract step works on hosts where PowerShell is policy-restricted
or simply absent (and to make `shell.disable_powershell` a meaningful
clud setting once that lands — issue tracking: forthcoming).

Usage:

    uv run --script extract_zip.py <archive.zip> <dest_dir>

The destination directory is created if it does not exist. Any path
traversal entries (`..` segments resolving outside dest) are rejected
before extraction so a malicious archive cannot write above dest.
"""

from __future__ import annotations

import os
import sys
import zipfile
from pathlib import Path


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(
            f"usage: {sys.argv[0]} <archive.zip> <dest_dir>",
            file=sys.stderr,
        )
        return 2
    archive = Path(argv[0])
    dest = Path(argv[1])
    if not archive.is_file():
        print(f"extract_zip: archive not found: {archive}", file=sys.stderr)
        return 1
    dest.mkdir(parents=True, exist_ok=True)
    dest_resolved = dest.resolve()
    with zipfile.ZipFile(archive) as zf:
        for member in zf.infolist():
            target = (dest_resolved / member.filename).resolve()
            if os.path.commonpath([dest_resolved, target]) != str(dest_resolved):
                print(
                    f"extract_zip: refusing path-traversal entry: {member.filename}",
                    file=sys.stderr,
                )
                return 1
        zf.extractall(dest_resolved)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
