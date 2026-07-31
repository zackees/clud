"""Reject hand-rolled cross toolchains; soldr owns Apple / Windows-MSVC (#637, #714).

soldr owns the blessed cross surface for `*-apple-darwin` and
`*-pc-windows-msvc`: `soldr prepare --target ...` provisions the LLVM toolchain
and the vendored MSVC CRT, and `soldr build --target ...` links against it.
`cargo xwin` and `cargo zigbuild --target *-apple-darwin` remain *technically*
reachable, and soldr's own docs call them legacy passthroughs — which is exactly
the failure mode this lint exists to prevent. Beyond correctness there is a
throughput argument: `cargo xwin` re-downloads and splats the MSVC CRT/SDK on
every cold cache, minutes at a time, while `soldr prepare` fetches a prepared
sysroot.

**Zig stays legitimate for Linux.** `aarch64-unknown-linux-gnu` cross-builds
through `cargo-zigbuild`, and the manylinux wheel links through `maturin --zig`.

## Two rule classes, and why the split matters (#714)

Splitting the table is the whole point of #714. The original rule required a
*literal* Apple/MSVC triple on the same line as the tool, which is right for Zig
(the same command aimed at Linux is the supported path) and wrong for everything
else: `cargo xwin build --target $TARGET` names no literal triple, and neither
does `cargo xwin build --release`, yet `xwin` is an MSVC-only tool by
construction. Those now fail unconditionally.

    BANNED_ALWAYS            tools with no legitimate target in this repo —
                             cargo-xwin, the bare `xwin` CLI, osxcross, `cross`
    BANNED_AT_SOLDR_TARGET   tools that are correct for Linux and wrong for
                             Apple/MSVC — cargo-zigbuild, `maturin --zig`, zig cc
    BANNED_INSTALLS          acquiring any of the above, at any target: a
                             hand-installed wrapper is what makes a hand-rolled
                             invocation reachable in the first place

## What this catches, and what it cannot

This is a **textual** scan, so it catches the shape a regression actually takes:
a literal command in YAML, Python, shell, PowerShell, TOML, Rust or a
Dockerfile. Install patterns additionally run against the whole file, because
the shape that matters in GitHub Actions spans lines:

    - uses: taiki-e/install-action@v2
      with:
        tool: cargo-xwin

Before #714 those patterns were compiled with `re.MULTILINE` but matched
per-line, so they could never fire.

The one hole left is a *conditional* tool driven at a target held in a variable
(`cargo zigbuild --target $TARGET`, legal for Linux, illegal for Apple). That is
closed separately and more strongly by
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

#: Triples soldr owns end-to-end. A conditional tool aimed at one of these is a
#: failure; the same tool aimed at Linux is the supported path.
#:
#: A concrete architecture is required. `*-apple-darwin` and `<triple>` appear
#: throughout these files in prose that *explains* the ban — matching the
#: wildcard would make the rule impossible to document.
SOLDR_OWNED_TARGET = re.compile(
    r"\b(?:x86_64|aarch64|arm64|i686|armv7)-(?:apple-darwin|pc-windows-msvc)\b"
)

#: Tools with no legitimate use in this repo at any target, so the target is not
#: consulted. `cargo xwin` and the bare `xwin` CLI only ever produce MSVC
#: artifacts; osxcross only ever produces Apple ones; `cross` drives its own
#: container toolchain images. soldr covers all three.
#:
#: The separator class is `["',\s-]+` rather than plain whitespace so the
#: argv-list form is caught too: `["cargo", "xwin", "build"]` is the same
#: command as `cargo xwin build`, and "move it from YAML into Python" must not
#: be a way around the rule.
BANNED_ALWAYS: tuple[tuple[str, re.Pattern[str]], ...] = (
    ("cargo xwin", re.compile(r"""\bcargo["',\s-]+xwin\b""")),
    # The standalone splatter, which is what actually downloads the CRT.
    ("xwin CLI", re.compile(r"""\bxwin["',\s]+(?:splat|download|unpack|list)\b""")),
    ("XWIN_* environment", re.compile(r"\bXWIN_[A-Z0-9_]+")),
    ("osxcross", re.compile(r"\bosxcross\b")),
    ("cross", re.compile(r"""\bcross["',\s]+build\b""")),
    ("Cross.toml", re.compile(r"\bCross\.toml\b")),
)

#: Correct for `*-unknown-linux-*`, wrong for the triples soldr owns. Only
#: flagged when a soldr-owned triple appears on the same line.
BANNED_AT_SOLDR_TARGET: tuple[tuple[str, re.Pattern[str]], ...] = (
    ("cargo zigbuild", re.compile(r"""\bcargo["',\s-]+zigbuild\b""")),
    ("zig cc / zig c++ / zig build", re.compile(r"""\bzig["',\s]+(?:cc|c\+\+|build)\b""")),
    ("maturin --zig", re.compile(r"--zig\b")),
)

#: Provisioning a cross toolchain by hand, at any target. soldr owns toolchain
#: acquisition for the crossed triples, and a hand-installed wrapper is what
#: makes a hand-rolled invocation possible in the first place — so the install
#: is banned even though the install line names no target.
#:
#: Matched against the whole (comment-stripped) file, not line by line: the
#: `taiki-e/install-action` shape puts the tool name on a later line. The
#: `(?:\n[^\n]*){0,4}?` window is deliberately bounded — an unbounded `.*?`
#: would pair an install-action step at the top of a workflow with an unrelated
#: mention hundreds of lines below.
BANNED_INSTALLS: tuple[tuple[str, re.Pattern[str]], ...] = (
    (
        "cargo install cargo-xwin",
        re.compile(r"cargo\s+(?:install|binstall)\s+[^\n]*\b(?:cargo-)?xwin\b"),
    ),
    (
        "cargo install cargo-zigbuild",
        re.compile(r"cargo\s+(?:install|binstall)\s+[^\n]*\bcargo-zigbuild\b"),
    ),
    (
        "taiki-e/install-action: cargo-xwin",
        re.compile(r"taiki-e/install-action[^\n]*(?:\n[^\n]*){0,4}?\bcargo-xwin\b"),
    ),
    (
        "taiki-e/install-action: cargo-zigbuild",
        re.compile(r"taiki-e/install-action[^\n]*(?:\n[^\n]*){0,4}?\bcargo-zigbuild\b"),
    ),
    ("pip install ziglang", re.compile(r"pip3?\s+install\s+[^\n]*\bziglang\b")),
    ("brew install zig", re.compile(r"brew\s+install\s+[^\n]*\bzig(?:lang)?\b")),
    (
        "system package manager: zig",
        re.compile(
            r"\b(?:apt-get|apt|dnf|yum|apk|pacman|choco|scoop|winget)\s+"
            r"(?:[-\w]+\s+)*(?:install|add|-S)\s+[^\n]*\bzig(?:lang)?\b"
        ),
    ),
    ("goto-bus-stop/setup-zig", re.compile(r"goto-bus-stop/setup-zig")),
    ("mlugg/setup-zig", re.compile(r"mlugg/setup-zig")),
    ("houseabsolute/actions-rust-cross", re.compile(r"houseabsolute/actions-rust-cross")),
    ("cross-rs/cross", re.compile(r"\bcross-rs/cross\b")),
    ("osxcross checkout", re.compile(r"tpoechtrager/osxcross")),
    (
        "hand-rolled linker for a soldr-owned target",
        re.compile(
            r"\[target\.(?:x86_64|aarch64|arm64|i686|armv7)-"
            r"(?:apple-darwin|pc-windows-msvc)\][^\[]*?\blinker\s*="
        ),
    ),
)

#: Trees whose contents drive builds, releases and developer toolchains.
#:
#: `.claude/hooks` rather than `.claude`: the latter also holds `worktrees/`,
#: an ignored full checkout of the repo, which would be walked twice and would
#: report violations at paths that do not exist on `main`.
SCAN_DIRS: tuple[str, ...] = (
    ".github",
    "ci",
    "bench",
    "dylints",
    "skills",
    ".claude/hooks",
    "crates/clud-bin/assets/tools",
)

#: Build / release / install entrypoints that are not under a scanned
#: directory. Several are extensionless on purpose — these are the `bash build`
#: style scripts. Listed by path, so a file that does not exist is simply
#: skipped (`.cargo/config.toml` is pre-registered for the day someone adds it,
#: since a linker override there is a hand-rolled cross toolchain).
SCAN_FILES: tuple[str, ...] = (
    "build",
    "lint",
    "test",
    "install",
    "install.sh",
    "install.ps1",
    "publish",
    ".cargo/config.toml",
)

SCAN_SUFFIXES: tuple[str, ...] = (".yml", ".yaml", ".py", ".sh", ".ps1", ".toml", ".rs")

#: Directories never worth walking, at any depth.
SKIP_DIR_NAMES: frozenset[str] = frozenset(
    {"__pycache__", "node_modules", "target", ".venv", ".git"}
)

#: This file names every banned tool by construction, as do the fixtures that
#: prove it rejects them and the doc that states the invariant.
EXEMPT_PATHS: frozenset[str] = frozenset(
    {
        "ci/banned_cross_tools.py",
        "tests/test_banned_cross_tools.py",
        "docs/architecture/ci.md",
    }
)


def _is_scannable(path: Path) -> bool:
    """Suffix match, plus Dockerfiles, which carry no extension."""
    return path.suffix in SCAN_SUFFIXES or path.name.startswith("Dockerfile")


def _iter_files() -> list[Path]:
    files: list[Path] = []
    for name in SCAN_DIRS:
        directory = ROOT / name
        if not directory.is_dir():
            continue
        for path in sorted(directory.rglob("*")):
            if not path.is_file() or not _is_scannable(path):
                continue
            if SKIP_DIR_NAMES.intersection(path.relative_to(ROOT).parts[:-1]):
                continue
            files.append(path)
    for name in SCAN_FILES:
        path = ROOT / name
        if path.is_file():
            files.append(path)
    return files


#: Suffixes whose line comments start with `#`. The empty suffix covers the
#: extensionless `bash build` style entrypoints, and `Dockerfile` is handled by
#: the same rule.
HASH_COMMENT_SUFFIXES = frozenset({".yml", ".yaml", ".py", ".sh", ".ps1", ".toml", ""})


#: Per-line escape hatch. Line comments cover most prose about the ban, but not
#: all of it — a module docstring is prose that no comment-stripper sees, and
#: the #714 unconditional rules made that collision likely rather than
#: theoretical. A line carrying this marker is skipped entirely. It is
#: deliberately verbose and greppable: `rg 'cross-lint: allow'` lists every
#: escape in the tree, so review can see them all at once.
ALLOW_MARKER = "cross-lint: allow"


def _strip_comment(line: str, suffix: str) -> str:
    """Drop a trailing comment so prose about the ban is not a violation.

    Comments in these files routinely *explain* why `cargo xwin` is forbidden;
    treating that as a violation would make the rule undocumentable.
    """
    if ALLOW_MARKER in line:
        return ""
    if suffix == ".rs":
        return line.split("//", 1)[0]
    if suffix in HASH_COMMENT_SUFFIXES:
        return line.split("#", 1)[0]
    return line


def _install_reason(tool: str) -> str:
    return (
        f"installs `{tool}` directly; soldr provisions the cross toolchain via "
        "`soldr prepare --target ...` (see docs/architecture/ci.md)"
    )


def scan_text(text: str, suffix: str = ".py") -> list[tuple[int, str, str]]:
    """Return `(line_number, tool, reason)` for every violation in `text`.

    Pure so the fixtures can exercise it directly, without writing files.
    """
    violations: list[tuple[int, str, str]] = []
    stripped = [_strip_comment(raw, suffix) for raw in text.splitlines()]

    for number, line in enumerate(stripped, start=1):
        if not line.strip():
            continue

        target = SOLDR_OWNED_TARGET.search(line)
        # These rules do not consult the target, but naming the literal triple
        # when the line happens to carry one makes the fix copy-pasteable.
        named = target.group(0) if target else "<triple>"

        for tool, pattern in BANNED_ALWAYS:
            if pattern.search(line):
                violations.append(
                    (
                        number,
                        tool,
                        f"drives `{tool}`, which has no supported target here; "
                        "soldr owns Apple/MSVC cross builds — use `soldr build "
                        f"--target {named}` (see docs/architecture/ci.md)",
                    )
                )

        if target:
            for tool, pattern in BANNED_AT_SOLDR_TARGET:
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

    # Installs run against the whole file: the shape that matters in GitHub
    # Actions puts the tool name on a line after the action reference. Offsets
    # map back to a line number by counting the newlines that precede them.
    joined = "\n".join(stripped)
    for tool, pattern in BANNED_INSTALLS:
        for match in pattern.finditer(joined):
            number = joined.count("\n", 0, match.start()) + 1
            violations.append((number, tool, _install_reason(tool)))

    # Sorted and de-duplicated: overlapping patterns (`cargo install
    # cargo-xwin` also matching the bare-CLI rule) should read as one finding.
    return sorted(set(violations))


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
