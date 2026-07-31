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
    # The standalone splatter, which is what actually downloads the CRT. Flags
    # sit between the binary and its subcommand in every real invocation
    # (`xwin --accept-license splat --output ...`, which is the form xwin's own
    # README uses), so requiring adjacency would miss the shape that matters.
    # Only flag-shaped tokens may sit between the binary and its subcommand,
    # and the binary may not be the tail of a path. `[^\n]*?` instead would
    # flag `PATHS = ["/opt/xwin", "list"]` and `"the xwin cache does not
    # download automatically"` — `list` and `download` are ordinary words.
    (
        "xwin CLI",
        re.compile(
            r"""(?<![/\\\w-])xwin\b(?:["',\s]+-{1,2}[\w-]+(?:[=\s]+[^\s"',]+)?)*"""
            r"""["',\s]+(?:splat|download|unpack|list)\b"""
        ),
    ),
    # `CARGO_XWIN_*` is cargo-xwin's own env prefix and is the half a real user
    # sets. `\b` does not match between `CARGO_` and `XWIN_` — `_` is a word
    # character — so the prefix has to be spelled out rather than assumed.
    ("XWIN_* environment", re.compile(r"\b(?:CARGO_)?XWIN_[A-Z0-9_]+")),
    ("osxcross", re.compile(r"\bosxcross\b")),
    # `cross` is an ordinary English word, so unlike the others this one is
    # anchored at a command position: start of line, a shell/Dockerfile
    # continuation, or a YAML `run:`. Without that anchor `name: cross build
    # matrix` and `let msg = "cross build failed";` both fail the lint, which
    # is how a rule earns a revert. The argv-list form needs its own pattern
    # because there the quotes *are* the anchor.
    (
        "cross",
        re.compile(
            r"""(?:^|[|&;(]|\bRUN\s|\brun:\s*)\s*"""
            # Wrappers that take a command as their argument: `sudo cross
            # build`, `env RUSTFLAGS=-C cross build`, `xargs cross build`,
            # `timeout 60 cross build`. Each may carry its own arguments.
            r"""(?:(?:sudo|env|xargs|timeout|nice|exec|command|time)\s+"""
            r"""(?:[-\w=./]+\s+)*)?"""
            r"""["']?cross["',\s]+"""
            r"""(?:\+\S+["',\s]+)?(?:build|test|run|check|rustc|bench|clippy)\b"""
        ),
    ),
    # The quoted argv form, where the quotes are the anchor. Flags and a
    # toolchain override may precede the verb, so the same verb list as the
    # shell rule stays reachable: `["cross", "+nightly", "build"]`.
    (
        "cross (argv list)",
        # Intervening arguments may be unquoted variables, not just string
        # literals — `["cross", "--target", target, "build"]` is the shape a
        # real script builds. Bounded and `]`-stopped so the scan cannot run
        # past the end of the list it started in.
        re.compile(
            r"""["']cross["']\s*,\s*(?:[^,\n\]]*,\s*){0,6}?"""
            r"""["'](?:build|test|run|check|rustc|bench|clippy)["']"""
        ),
    ),
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
    # The plain crates.io install. `(?!-)` so `cargo install cross-something`
    # — an unrelated crate that merely starts with the word — stays legal.
    (
        "cargo install cross",
        re.compile(r"cargo\s+(?:install|binstall)\s+[^\n]*\bcross\b(?!-)"),
    ),
    ("osxcross checkout", re.compile(r"tpoechtrager/osxcross")),
    (
        "hand-rolled linker for a soldr-owned target",
        # The key may be quoted (`[target."x86_64-pc-windows-msvc"]` is equally
        # idiomatic TOML) and `linker` need not be the section's first key. The
        # window therefore runs to the next section *header* — a `[` at the
        # start of a line — rather than to the next `[` of any kind, which an
        # ordinary array value like `rustflags = ["-C", ...]` would otherwise
        # terminate.
        re.compile(
            r"""\[target\.["']?(?:x86_64|aarch64|arm64|i686|armv7)-"""
            r"""(?:apple-darwin|pc-windows-msvc)["']?\]"""
            r"(?:(?!\n\s*\[)[\s\S])*?\blinker\s*="
        ),
    ),
)

#: Trees whose contents drive builds, releases and developer toolchains.
#:
#: `.claude/hooks` rather than `.claude`: the latter also holds `worktrees/`,
#: an ignored full checkout of the repo, which would be walked twice and would
#: report violations at paths that do not exist on `main`.
#: `crates` covers the product source, not just the asset scripts under it:
#: clud shells out to build commands, so a `cargo xwin` in Rust is a real
#: vector and "everywhere" would be a lie without it. `vendor/` stays out —
#: it is third-party source we do not author, and `whisper-rs-sys/build.rs`
#: legitimately reasons about zig's C++ runtime for the Linux lanes.
SCAN_DIRS: tuple[str, ...] = (
    ".github",
    "ci",
    "bench",
    "crates",
    "dylints",
    "skills",
    "testbins",
    "tests",
    ".claude/hooks",
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
#:
#: **Strictly line-scoped**, including inside a multi-line docstring: the
#: marker must sit on the same line as the tool name, not on the docstring's
#: closing line. A block-scoped marker would need to guess where the block
#: ends, and guessing wrong silences more than the author asked for.
ALLOW_MARKER = "cross-lint: allow"


def _strip_rust_comments(text: str) -> str:
    """Blank out Rust comments, character for character.

    A scanner rather than a regex, because all three of the cheap regex
    approaches are wrong in a way that matters:

    * `/\\*.*?\\*/` stops at the first `*/`, but **Rust block comments nest**,
      so `/* /* */ cargo xwin build */` would leave live-looking text behind.
    * Splitting a line at `//` cuts strings containing `//` — a URL such as
      `"https://github.com/rust-cross/cargo-xwin"` is a real reference to the
      banned tool and must survive to be reported, and truncating there also
      hides whatever follows on the line.
    * Neither knows about string literals, so `let open = "/*";` … `let close
      = "*/";` would swallow every line between them. That is a one-line
      bypass for anyone who notices.

    Comment characters become spaces and newlines are preserved, so every
    offset and line number in the result matches the input exactly. The
    install patterns map a match offset back to a line number, so that
    property is load-bearing rather than incidental.

    Known limit: `'` is not treated as a delimiter, because Rust lifetimes
    (`&'a str`) would open a string that never closes. A char literal holding
    a quote (`'"'`) therefore desynchronises the scanner — always in the
    direction of stripping *less*, which costs a false positive, never a
    missed violation.
    """
    out = list(text)
    length = len(text)
    index = 0
    depth = 0
    in_string = False
    in_line_comment = False
    while index < length:
        char = text[index]
        following = text[index + 1] if index + 1 < length else ""
        if in_line_comment:
            if char == "\n":
                in_line_comment = False
            else:
                out[index] = " "
            index += 1
        elif depth:
            if char == "/" and following == "*":
                depth += 1
                out[index] = out[index + 1] = " "
                index += 2
            elif char == "*" and following == "/":
                depth -= 1
                out[index] = out[index + 1] = " "
                index += 2
            else:
                if char != "\n":
                    out[index] = " "
                index += 1
        elif in_string:
            if char == "\\":
                index += 2
                continue
            if char == '"':
                in_string = False
            index += 1
        elif char == '"':
            in_string = True
            index += 1
        elif char == "/" and following == "/":
            in_line_comment = True
            out[index] = " "
            index += 1
        elif char == "/" and following == "*":
            depth = 1
            out[index] = out[index + 1] = " "
            index += 2
        else:
            index += 1
    return "".join(out)


def _strip_comment(line: str, suffix: str) -> str:
    """Drop a trailing comment so prose about the ban is not a violation.

    Comments in these files routinely *explain* why `cargo xwin` is forbidden;
    treating that as a violation would make the rule undocumentable. Rust is
    absent here because `_strip_rust_comments` has already blanked its
    comments, line and block alike, before the text was split.
    """
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
    # The marker is read from the *original* line. Blanking Rust comments
    # first would erase a marker written as a trailing `// cross-lint: allow`,
    # which is where anyone would naturally put one.
    original = text.splitlines()
    if suffix == ".rs":
        text = _strip_rust_comments(text)
    stripped = [
        "" if ALLOW_MARKER in raw else _strip_comment(line, suffix)
        # strict: `_strip_rust_comments` blanks characters in place rather
        # than removing them, so the two line lists are the same length by
        # construction. If that ever stops being true, failing here is far
        # better than silently misaligning every marker with the wrong line.
        for raw, line in zip(original, text.splitlines(), strict=True)
    ]

    # Installs run first because they are the more specific finding: a line
    # reading `cargo install cargo-xwin` is an install, and also — to the
    # invocation rules — a mention of `cargo xwin`. Reporting both would count
    # one mistake twice in the summary total and make the fix look bigger than
    # it is, so an install suppresses the invocation rules across every line it
    # spans — not just the line it starts on. The canonical GitHub Actions
    # shape puts `uses: taiki-e/install-action` and `tool: cargo-xwin` on
    # different lines, so suppressing only the start line would leave the
    # `tool:` line to be reported a second time by the `cargo xwin` rule,
    # which is the exact double-count this is here to prevent.
    #
    # Matched against the whole file, not line by line, for the same reason.
    # Offsets map back to a line number by counting the newlines that precede
    # them.
    joined = "\n".join(stripped)
    install_lines: set[int] = set()
    for tool, pattern in BANNED_INSTALLS:
        for match in pattern.finditer(joined):
            number = joined.count("\n", 0, match.start()) + 1
            last = joined.count("\n", 0, match.end()) + 1
            install_lines.update(range(number, last + 1))
            violations.append((number, tool, _install_reason(tool)))

    for number, line in enumerate(stripped, start=1):
        if number in install_lines:
            continue
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

    # Sorted and de-duplicated: two spellings of the same rule matching the
    # same line should read as one finding.
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
