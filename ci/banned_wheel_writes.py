"""Only `ci/wheel_rewrite.py` may write a zip or call `extractall` in ci/ (#1545).

`ZipFile.extractall` drops Unix modes, and each hand-rolled repack derived
the entry mode its own way. That is how 2.8.14 and 2.8.20 shipped every
`.data/scripts/*` binary as 0644 (#1544). `ci/wheel_rewrite.py` is the single
writer: kept entries reuse their original ZipInfo and added entries must
state a mode. Everything else in ci/ reads wheels only.

A line carrying ``wheel-write-lint: allow`` is exempt; use it only for an
archive that is not a wheel (e.g. the tar test bundle), and say why.

Run via `bash lint` (see `ci/lint.py`).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CI_DIR = ROOT / "ci"

ALLOW_MARKER = "wheel-write-lint: allow"
EXEMPT_FILES = frozenset({"wheel_rewrite.py", "banned_wheel_writes.py"})

EXTRACTALL = re.compile(r"\bextractall\s*\(")
#: `zipfile.ZipFile(path, "w")`, `ZipFile(p, mode="a")`, across lines too.
ZIP_WRITE = re.compile(
    r"\bZipFile\s*\((?:[^()]|\([^()]*\))*?,\s*(?:mode\s*=\s*)?[\"'][wax][\"']",
    re.DOTALL,
)

REASON = (
    "wheel writes go through ci/wheel_rewrite.py (rewrite_wheel / write_wheel), "
    "which preserves each entry's Unix mode; extractall + repack dropped the "
    "exec bit and shipped a non-executable `clud` (#1544)."
)


def scan(text: str) -> list[int]:
    """1-based line numbers of banned constructs not marked as allowed."""
    lines = text.splitlines()
    hits: set[int] = set()
    for pattern in (EXTRACTALL, ZIP_WRITE):
        for match in pattern.finditer(text):
            line = text.count("\n", 0, match.start()) + 1
            if ALLOW_MARKER not in lines[line - 1]:
                hits.add(line)
    return sorted(hits)


def main() -> int:
    failures = 0
    for path in sorted(CI_DIR.rglob("*.py")):
        if path.name in EXEMPT_FILES:
            continue
        for line in scan(path.read_text(encoding="utf-8")):
            print(f"{path.relative_to(ROOT).as_posix()}:{line}: BANNED -- {REASON}")
            failures += 1
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
