"""clud resolves the user's home directory in exactly one place (#1836).

`crates/clud-bin/src/home.rs::user_home` owns the precedence (Windows:
`USERPROFILE`, then `HOME`, then the OS profile folder; elsewhere: `HOME`, then
the OS lookup). Any second resolver can disagree with it under an isolated or
redirected home, and the disagreement only shows on Windows or in tests. #1829
shipped exactly that: the managed DeepSeek Harness installed under
`dirs::home_dir()` (the Windows Known Folder) while discovery searched
`USERPROFILE`, and it took native CI to notice.

Two rules, scanned over tracked Rust sources:

* ``dirs::home_dir`` / ``std::env::home_dir`` / ``home::home_dir`` (called or
  passed as a function) are banned outside ``home.rs``.
* Reading the ``USERPROFILE`` variable is banned outside ``ALLOWED_FILES``.
  Every Windows-correct home helper has to read it, so this catches a new
  private copy of the rule, however it is spelled.

Tests may read and set these variables to build isolated homes; files named
``*_tests.rs`` and anything under a ``tests/`` directory are not scanned.
The Dylint lint ``ban_dirs_home_dir`` enforces the first rule on resolved
paths, which also catches renamed imports. This guard runs in ``bash lint`` on
every PR, while Dylint runs off the PR path.

Run via `bash lint` (see `ci/lint.py`).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

from ci.process import check_output

ROOT = Path(__file__).resolve().parents[1]

RESOLVER = "crates/clud-bin/src/home.rs"

#: `dirs::home_dir`, `std::env::home_dir`, `env::home_dir`, `home::home_dir`.
HOME_DIR_CALL = re.compile(r"\b(?:dirs|dirs_next|env|home)::home_dir\b")
TEST_MODULE = re.compile(r"^\s*mod tests\b")
USERPROFILE_READ = re.compile(r"""\bvar(?:_os)?\(\s*"USERPROFILE"\s*\)""")

#: Files that may read `USERPROFILE` directly, each for a stated reason.
ALLOWED_FILES = frozenset(
    {
        # The resolver itself.
        RESOLVER,
        # `InstallPathEnv` snapshots the raw variables so bootstrap tests can
        # inject them; its consumers resolve through `home::resolve`.
        "crates/clud-bin/src/backend_bootstrap.rs",
        # `tap` is a separate one-dependency binary that cannot link clud; its
        # private helper mirrors `home::resolve`'s precedence.
        "crates/tap/src/main.rs",
    }
)

REASON = (
    "resolve the user's home with `crate::home::user_home()` (or "
    "`clud::home::user_home()` from a binary). A second resolver disagrees "
    "with it under an isolated or redirected home, on Windows especially "
    "(#1829, #1836)."
)


def tracked_rust_files() -> list[str]:
    out = check_output(["git", "ls-files", "-z", "--", "*.rs"], cwd=ROOT)
    return [p for p in str(out).split("\0") if p]


def is_scanned(rel: str) -> bool:
    if rel.startswith(("vendor/", "dylints/")):
        return False
    if rel.endswith("_tests.rs") or "/tests/" in f"/{rel}":
        return False
    return True


def strip_line_comment(line: str) -> str:
    """Drop a trailing `//` comment so prose naming the ban does not trip it."""
    in_string = False
    previous = ""
    for index, char in enumerate(line):
        if char == '"' and previous != "\\":
            in_string = not in_string
        if not in_string and line.startswith("//", index):
            return line[:index]
        previous = char
    return line


def scan(rel: str, text: str) -> list[tuple[int, str, str]]:
    """(line number, rule, line) for each violation in one file."""
    violations = []
    previous = ""
    for number, raw in enumerate(text.splitlines(), start=1):
        if TEST_MODULE.match(raw) and previous.strip() == "#[cfg(test)]":
            # The unit-test module closes the file by convention; tests may
            # save and restore these variables to build an isolated home.
            break
        if raw.strip():
            previous = raw
        line = strip_line_comment(raw)
        if rel != RESOLVER and HOME_DIR_CALL.search(line):
            violations.append((number, "home_dir call", raw.strip()))
        if rel not in ALLOWED_FILES and USERPROFILE_READ.search(line):
            violations.append((number, "raw USERPROFILE read", raw.strip()))
    return violations


def main() -> int:
    total = 0
    for rel in tracked_rust_files():
        if not is_scanned(rel):
            continue
        try:
            text = (ROOT / rel).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        for number, rule, line in scan(rel, text):
            print(f"{rel}:{number}: BANNED {rule} — {REASON}", file=sys.stderr)
            print(f"  {line}", file=sys.stderr)
            total += 1
    if total:
        print(
            f"\n{total} home-directory resolution(s) outside {RESOLVER}.",
            file=sys.stderr,
        )
        return 1
    print("Home directory resolved only in home.rs.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
