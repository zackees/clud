"""Fixtures for the #637 cross-compiler linter.

The linter's value is entirely in what it *rejects*, so the negative fixtures
are the point. The positive ones matter just as much in the other direction: a
rule that also rejects the Linux Zig path would be reverted within a day, and
Zig is the supported cross for `*-unknown-linux-gnu` and for the manylinux
wheel.

Fixtures are strings rather than files so a future command shape can be added
here in one line, and so the same table covers YAML, Python and shell — the
issue's "cannot bypass the rule by moving from YAML to Python or shell".
"""

from __future__ import annotations

import pytest

from ci.banned_cross_tools import main, scan_text

# --------------------------------------------------------------- rejected --

REJECTED: list[tuple[str, str, str]] = [
    (
        "cargo-xwin invocation, windows x86",
        "cargo xwin build --release --target x86_64-pc-windows-msvc",
        ".sh",
    ),
    (
        "cargo-xwin hyphenated form, windows arm",
        "cargo-xwin build --target aarch64-pc-windows-msvc",
        ".sh",
    ),
    (
        "zigbuild aimed at darwin arm",
        "cargo zigbuild --target aarch64-apple-darwin",
        ".sh",
    ),
    (
        "zigbuild aimed at darwin x86",
        "cargo-zigbuild build --target x86_64-apple-darwin",
        ".sh",
    ),
    (
        "maturin --zig aimed at darwin",
        "maturin build --zig --target aarch64-apple-darwin --release",
        ".sh",
    ),
    (
        "zig cc as the linker for an MSVC target",
        'env["CC_x86_64_pc_windows_msvc"] = "zig cc -target x86_64-pc-windows-msvc"',
        ".py",
    ),
    (
        "the `cross` wrapper at a darwin target",
        "cross build --target x86_64-apple-darwin",
        ".sh",
    ),
    (
        "osxcross for darwin",
        "osxcross-clang --target aarch64-apple-darwin",
        ".sh",
    ),
    # The rule must not be evadable by changing file type. Same command, three
    # syntaxes.
    (
        "same violation expressed in YAML",
        "        run: cargo xwin build --target x86_64-pc-windows-msvc",
        ".yml",
    ),
    (
        "same violation expressed in Python",
        'subprocess.run(["cargo", "xwin", "build", "--target", "x86_64-pc-windows-msvc"])',
        ".py",
    ),
    # Installs are banned at any target: a hand-installed wrapper is what makes
    # a hand-rolled invocation reachable.
    ("cargo install cargo-xwin", "cargo install cargo-xwin --locked", ".sh"),
    ("cargo install cargo-zigbuild", "cargo install cargo-zigbuild", ".sh"),
    ("pip install ziglang", "pip install ziglang==0.13.0", ".sh"),
    ("setup-zig action", "      - uses: goto-bus-stop/setup-zig@v2", ".yml"),
    ("mlugg setup-zig action", "      - uses: mlugg/setup-zig@v1", ".yml"),
]


@pytest.mark.parametrize(
    ("line", "suffix"),
    [(line, suffix) for _, line, suffix in REJECTED],
    ids=[name for name, _, _ in REJECTED],
)
def test_direct_cross_tooling_is_rejected(line: str, suffix: str):
    violations = scan_text(line, suffix)
    assert violations, f"linter did not reject: {line}"


# --------------------------------------------------------------- accepted --

ACCEPTED: list[tuple[str, str, str]] = [
    (
        "soldr prepare for windows",
        "soldr prepare --target x86_64-pc-windows-msvc",
        ".sh",
    ),
    (
        "soldr build for darwin",
        "soldr build --release --target aarch64-apple-darwin",
        ".sh",
    ),
    (
        "soldr build argv assembled in Python",
        'return ["soldr", "build", *subcommand[1:], "--target", target]',
        ".py",
    ),
    # Zig is the supported cross for Linux and must stay untouched.
    (
        "zigbuild for linux arm",
        "cargo zigbuild --target aarch64-unknown-linux-gnu",
        ".sh",
    ),
    (
        "maturin --zig for the manylinux wheel",
        "maturin build --zig --compatibility manylinux2014 "
        "--target x86_64-unknown-linux-gnu",
        ".sh",
    ),
    (
        "zigbuild argv assembled in Python for a linux target",
        'return [PACKAGE_MANAGER, "zigbuild", *subcommand[1:], "--target", target]',
        ".py",
    ),
    (
        "the matrix strategy name alone names no target",
        "      strategy: zigbuild",
        ".yml",
    ),
    # Prose explaining the ban must not trip it, or the rule becomes
    # undocumentable.
    (
        "a comment describing the forbidden command",
        "# never run `cargo xwin build --target x86_64-pc-windows-msvc` here",
        ".py",
    ),
    (
        "docstring prose using the wildcard form",
        "`cargo xwin` / `cargo zigbuild --target *-apple-darwin` are legacy.",
        ".py",
    ),
    (
        "plain cargo at an MSVC target is fine — clippy does not link",
        "return ['cargo', *subcommand, '--target', 'x86_64-pc-windows-msvc']",
        ".py",
    ),
]


@pytest.mark.parametrize(
    ("line", "suffix"),
    [(line, suffix) for _, line, suffix in ACCEPTED],
    ids=[name for name, _, _ in ACCEPTED],
)
def test_supported_paths_are_accepted(line: str, suffix: str):
    violations = scan_text(line, suffix)
    assert not violations, f"linter wrongly rejected: {line} -> {violations}"


def test_the_repository_is_currently_clean():
    """The lint runs in `bash lint` and in CI's static job, so a red tree here
    is a red build. Asserting it separately means a failure names *this* rule
    rather than surfacing as a generic lint exit code."""
    assert main() == 0


def test_a_violation_reports_file_line_tool_and_the_soldr_replacement():
    """The issue asks for an actionable failure, not a boolean. A reader who
    has never seen this rule needs to know what to write instead."""
    text = "\n".join(
        [
            "steps:",
            "  - run: cargo xwin build --target x86_64-pc-windows-msvc",
        ]
    )
    (number, tool, reason) = scan_text(text, ".yml")[0]
    assert number == 2, "line number must point at the offending line"
    assert tool == "cargo xwin"
    assert "x86_64-pc-windows-msvc" in reason
    assert "soldr build --target" in reason, "must name the replacement"
    assert "docs/architecture/ci.md" in reason, "must point at the invariant"
