"""Reject direct cross-compiler use for Apple / Windows-MSVC targets (#637).

soldr owns the blessed cross surface for `*-apple-darwin` and
`*-pc-windows-msvc`: `soldr prepare --target ...` provisions the LLVM toolchain
and the vendored MSVC CRT, and `soldr build --target ...` links against it.
`cargo xwin` and `cargo zigbuild --target *-apple-darwin` remain *technically*
reachable, and soldr's own docs call them legacy passthroughs — which is exactly
the failure mode this lint exists to prevent. A future workflow, release script
or build helper could reach for one and nothing would notice: `ci_matrix.py`'s
`strategy` field is unit-tested, but the strategy is not what runs the compiler.

**Zig stays legitimate for Linux.** `aarch64-unknown-linux-gnu` cross-builds
through `cargo-zigbuild`, and the manylinux wheel links through `maturin --zig`.
The rule is therefore *target-aware*: a banned tool is only banned when it is
pointed at an Apple or MSVC triple.

## What this catches, and what it cannot

This is a **textual** scan, so it catches the shape a regression actually takes:
a literal command with a literal target, in YAML, Python or shell. It cannot
follow a target held in a variable.

That hole is closed separately and more strongly by
`tests/test_ci_matrix.py::test_cross_argv_never_routes_apple_or_msvc_through_a_banned_tool`,
which calls the real `ci.xbuild.cargo_argv` for every target in the matrix and
asserts the argv it returns names no banned tool. Text scan for the shapes,
behavioural assertion for the dispatch — neither alone is enough.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

#: Triples soldr owns end-to-end. A banned tool aimed at one of these is a
#: failure; the same tool aimed at Linux is the supported path.
#:
#: A concrete architecture is required. `*-apple-darwin` and `<triple>` appear
#: throughout these files in prose that *explains* the ban — matching the
#: wildcard would make the rule impossible to document, and a real regression
#: names a real triple (or a variable, which the behavioural test in
#: `tests/test_ci_matrix.py` covers instead).
SOLDR_OWNED_TARGET = re.compile(
    r"\b(?:x86_64|aarch64|arm64|i686|armv7)-(?:apple-darwin|pc-windows-msvc)\b"
)

#: Cross wrappers and compilers that must not be driven at a soldr-owned
#: target. Keyed by the display name used in the failure message.
#:
#: The separator class is `["',\s-]+` rather than plain whitespace so the
#: argv-list form is caught too: `["cargo", "xwin", "build"]` is the same
#: command as `cargo xwin build`, and "move it from YAML into Python" must not
#: be a way around the rule.
BANNED_INVOCATIONS: tuple[tuple[str, re.Pattern[str]], ...] = (
    ("cargo xwin", re.compile(r"""\bcargo["',\s-]+xwin\b""")),
    ("cargo zigbuild", re.compile(r"""\bcargo["',\s-]+zigbuild\b""")),
    ("zig cc / zig c++ / zig build", re.compile(r"""\bzig["',\s]+(?:cc|c\+\+|build)\b""")),
    ("maturin --zig", re.compile(r"--zig\b")),
    ("cross", re.compile(r"\bcross\s+build\b")),
    ("osxcross", re.compile(r"\bosxcross\b")),
)

#: Provisioning a cross toolchain by hand, at any target. soldr owns toolchain
#: acquisition for the crossed triples, and a hand-installed wrapper is what
#: makes a hand-rolled invocation possible in the first place — so the install
#: is banned even though the install line names no target.
BANNED_INSTALLS: tuple[tuple[str, re.Pattern[str]], ...] = (
    ("cargo install cargo-xwin", re.compile(r"cargo\s+install\s+[^\n]*\bcargo-xwin\b")),
    (
        "cargo install cargo-zigbuild",
        re.compile(r"cargo\s+install\s+[^\n]*\bcargo-zigbuild\b"),
    ),
    (
        "taiki-e/install-action: cargo-xwin",
        re.compile(r"taiki-e/install-action[^\n]*\n?[^\n]*cargo-xwin", re.MULTILINE),
    ),
    (
        "taiki-e/install-action: cargo-zigbuild",
        re.compile(r"taiki-e/install-action[^\n]*\n?[^\n]*cargo-zigbuild", re.MULTILINE),
    ),
    ("pip install ziglang", re.compile(r"pip\s+install\s+[^\n]*\bziglang\b")),
    ("goto-bus-stop/setup-zig", re.compile(r"goto-bus-stop/setup-zig")),
    ("mlugg/setup-zig", re.compile(r"mlugg/setup-zig")),
)

#: Trees whose contents drive builds and releases.
SCAN_DIRS: tuple[str, ...] = (".github", "ci")

#: Build / release entrypoints at the repo root that are not under a scanned
#: directory. Extensionless on purpose — these are the `bash build` style
#: scripts.
SCAN_FILES: tuple[str, ...] = ("build", "lint", "test", "install", "clean")

SCAN_SUFFIXES: tuple[str, ...] = (".yml", ".yaml", ".py", ".sh", ".ps1", ".toml")

#: This file names every banned tool by construction, as do the fixtures that
#: prove it rejects them and the doc that states the invariant.
EXEMPT_PATHS: frozenset[str] = frozenset(
    {
        "ci/banned_cross_tools.py",
        "tests/test_banned_cross_tools.py",
        "docs/architecture/ci.md",
    }
)


def _iter_files() -> list[Path]:
    files: list[Path] = []
    for name in SCAN_DIRS:
        directory = ROOT / name
        if not directory.is_dir():
            continue
        files.extend(
            path
            for path in sorted(directory.rglob("*"))
            if path.is_file() and path.suffix in SCAN_SUFFIXES
        )
    for name in SCAN_FILES:
        path = ROOT / name
        if path.is_file():
            files.append(path)
    return files


def _strip_comment(line: str, suffix: str) -> str:
    """Drop a trailing comment so prose about the ban is not a violation.

    Comments in these files routinely *explain* why `cargo xwin` is forbidden;
    treating that as a violation would make the rule undocumentable. Only `#`
    comments are handled, which covers YAML, Python, shell and TOML — the
    suffixes actually scanned.
    """
    if suffix in (".yml", ".yaml", ".py", ".sh", ".toml"):
        return line.split("#", 1)[0]
    return line


def scan_text(text: str, suffix: str = ".py") -> list[tuple[int, str, str]]:
    """Return `(line_number, tool, reason)` for every violation in `text`.

    Pure so the fixtures can exercise it directly, without writing files.
    """
    violations: list[tuple[int, str, str]] = []
    for number, raw in enumerate(text.splitlines(), start=1):
        line = _strip_comment(raw, suffix)
        if not line.strip():
            continue

        target = SOLDR_OWNED_TARGET.search(line)
        if target:
            for tool, pattern in BANNED_INVOCATIONS:
                if pattern.search(line):
                    violations.append(
                        (
                            number,
                            tool,
                            f"drives `{tool}` at `{target.group(0)}`; soldr owns "
                            "Apple/MSVC cross builds — use `soldr build --target "
                            f"{target.group(0)}` (see docs/architecture/ci.md)",
                        )
                    )

        for tool, pattern in BANNED_INSTALLS:
            if pattern.search(line):
                violations.append(
                    (
                        number,
                        tool,
                        f"installs `{tool}` directly; soldr provisions the cross "
                        "toolchain via `soldr prepare --target ...` "
                        "(see docs/architecture/ci.md)",
                    )
                )
    return violations


def main() -> int:
    total = 0
    for path in _iter_files():
        rel = path.relative_to(ROOT).as_posix()
        if rel in EXEMPT_PATHS:
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        for number, _tool, reason in scan_text(text, path.suffix):
            print(f"{rel}:{number}: BANNED CROSS TOOL — {reason}", file=sys.stderr)
            total += 1

    if total:
        print(
            f"\n{total} banned cross-compiler usage(s) found. "
            "Apple and Windows-MSVC targets must go through soldr's blessed "
            "cross surface; Zig remains correct for *-unknown-linux-* only.",
            file=sys.stderr,
        )
        return 1
    print("No banned cross-compiler usage found.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
