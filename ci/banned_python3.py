"""Call the interpreter `python`, never `python3`.

clud normalizes the interpreter name. On Linux and macOS the system usually
ships only `python3`; on Windows the installer ships only `python` (with a
`python3` that is often the Microsoft Store stub, which opens the Store instead
of running anything). clud's shim (`clud-shim`, installed as `python` *and*
`python3` in `~/.clud/state/shims/` for every session) resolves both names to
the same interpreter, so inside clud `python` works on every platform.

`python3` in our own code undoes that: a hook, script or test that spells it
`python3` works on the author's Linux box and fails on Windows, or picks a
different interpreter than the one clud resolved. So every tracked file must
say `python`.

Exceptions, each for a reason the error cannot fix:

* ``EXEMPT_FILES``: the shim modules that *implement* the normalization, and
  so have to recognize `python3` as an input name.
* A line carrying ``python-name-lint: allow``: a name clud does not control
  (a Debian package, an interpreter inside a container image, the pre-clud
  installer, a guard that must recognize what an agent might type). Keep the
  marker on the offending line, or use ``python-name-lint: allow-next-line``
  on the line above when the offending line ends in a `\\` continuation.
  `rg 'python-name-lint'` lists every escape.

Run via `bash lint` (see `ci/lint.py`).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

from ci.process import check_output

ROOT = Path(__file__).resolve().parents[1]

#: Word-boundary match, so `python3`, `python3.13`, `python3-pip` and
#: `python3.exe` all count, but `cpython3x` identifiers do not.
PATTERN = re.compile(r"(?<![A-Za-z0-9_])python3(?![A-Za-z_])")

ALLOW_MARKER = "python-name-lint: allow"
NEXT_LINE_MARKER = "python-name-lint: allow-next-line"

#: Files that implement the `python` / `python3` normalization itself.
EXEMPT_FILES = frozenset(
    {
        "crates/clud-bin/src/shim_resolve.rs",
        "crates/clud-bin/src/bin/clud_shim.rs",
        "crates/clud-bin/src/shim_install.rs",
        "crates/clud-bin/src/shim_session.rs",
        "crates/clud-bin/src/shim_uv.rs",
        "crates/clud-bin/src/path_norm.rs",
        # This lint and its tests have to spell the banned name.
        "ci/banned_python3.py",
        "tests/test_banned_python3.py",
    }
)

#: Never scanned: third-party code and generated files.
SKIP_PREFIXES = ("vendor/",)
SKIP_SUFFIXES = (".lock", ".png", ".jpg", ".gif", ".ico", ".wasm", ".bin")

REASON = (
    "call the interpreter `python`, not `python3`. clud's shim installs both "
    "names and resolves them to one interpreter, but only `python` exists on "
    "every platform: stock Windows has no real `python3` (at best a Microsoft "
    "Store stub), so code that says `python3` works on Linux and breaks on "
    "Windows. If the name is outside clud's control (a distro package, a "
    "container image, the installer), add `python-name-lint: allow` to the line."
)


def tracked_files() -> list[str]:
    out = check_output(["git", "ls-files", "-z"], cwd=ROOT)
    return [p for p in str(out).split("\0") if p]


def scan(text: str) -> list[tuple[int, str]]:
    """Lines that name `python3` without the allow marker.

    The marker covers its own line, or, as ``allow-next-line``, the line
    after it. The second form is for lines that end in a shell or Dockerfile
    `\\` continuation, where a trailing comment would break the command.
    """
    violations = []
    previous = ""
    for number, line in enumerate(text.splitlines(), start=1):
        allowed = (ALLOW_MARKER in line and NEXT_LINE_MARKER not in line) or (
            NEXT_LINE_MARKER in previous
        )
        if PATTERN.search(line) and not allowed:
            violations.append((number, line.strip()))
        previous = line
    return violations


def is_scanned(rel: str) -> bool:
    return (
        rel not in EXEMPT_FILES
        and not rel.startswith(SKIP_PREFIXES)
        and not rel.endswith(SKIP_SUFFIXES)
    )


def main() -> int:
    total = 0
    for rel in tracked_files():
        if not is_scanned(rel):
            continue
        try:
            text = (ROOT / rel).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        for number, line in scan(text):
            print(f"{rel}:{number}: BANNED `python3` — {REASON}", file=sys.stderr)
            print(f"  {line}", file=sys.stderr)
            total += 1
    if total:
        print(
            f"\n{total} `python3` reference(s) found. Use `python`; clud's shim "
            "makes that name work on Linux, macOS and Windows.",
            file=sys.stderr,
        )
        return 1
    print("No `python3` references found.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
