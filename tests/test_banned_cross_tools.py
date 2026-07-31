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
    # ---------------------------------------------------------------- #714 --
    # Every one of these was verified to slip through the pre-#714 linter.
    # `xwin` is MSVC-only by construction, so requiring a literal triple on the
    # line made the two shapes a real workflow uses — a variable target, or a
    # target supplied elsewhere entirely — invisible.
    ("xwin at a variable target", "cargo xwin build --target $TARGET", ".sh"),
    ("xwin with no target on the line", "cargo xwin build --release", ".sh"),
    ("the bare xwin CLI splatting the CRT", "xwin splat --output /opt/xwin", ".sh"),
    ("XWIN_* environment", "XWIN_ACCEPT_LICENSE=1 cargo build", ".sh"),
    ("cargo binstall cargo-xwin", "cargo binstall cargo-xwin", ".sh"),
    ("cargo install the bare xwin CLI", "cargo install xwin --locked", ".sh"),
    ("brew install zig", "brew install zig", ".sh"),
    ("apt-get install zig", "apt-get install -y zig", ".sh"),
    ("choco install zig", "choco install zig -y", ".ps1"),
    ("actions-rust-cross", "      - uses: houseabsolute/actions-rust-cross@v1", ".yml"),
    ("cross-rs source install", "cargo install --git https://github.com/cross-rs/cross", ".sh"),
    ("osxcross checkout", "git clone https://github.com/tpoechtrager/osxcross", ".sh"),
    ("a checked-in Cross.toml", "cp Cross.toml /workspace/", ".sh"),
    # Rust source and Dockerfiles are build surfaces too; the empty suffix is a
    # Dockerfile or an extensionless `bash build` style entrypoint.
    ("xwin argv assembled in Rust", 'let argv = vec!["cargo", "xwin", "build"];', ".rs"),
    ("xwin install in a Dockerfile", "RUN cargo install cargo-xwin --locked", ""),
    # The multi-line GitHub Actions shape. The pre-#714 patterns were compiled
    # with re.MULTILINE but matched per line, so this could never fire.
    (
        "taiki-e/install-action with the tool on a later line",
        "      - uses: taiki-e/install-action@v2\n        with:\n          tool: cargo-xwin",
        ".yml",
    ),
    (
        "taiki-e/install-action naming cargo-zigbuild on a later line",
        "      - uses: taiki-e/install-action@v2\n        with:\n          tool: cargo-zigbuild",
        ".yml",
    ),
    (
        "a hand-rolled linker for a soldr-owned target",
        '[target.x86_64-pc-windows-msvc]\nlinker = "lld-link"',
        ".toml",
    ),
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
    # Wildcard prose alone is no longer enough for the unconditional rules
    # (#714): `cargo xwin` is a violation wherever it appears, target or not,
    # and a docstring is prose no comment-stripper can see. Such a line must
    # carry the marker — which is the honest trade, since the alternative is a
    # rule that cannot distinguish documentation from a regression.
    (
        "docstring prose naming a conditional tool at a wildcard target",
        "`cargo zigbuild --target *-apple-darwin` is a legacy passthrough.",
        ".py",
    ),
    (
        "docstring prose naming an unconditional tool, with the marker",
        "`cargo xwin` is the legacy passthrough. (cross-lint: allow)",
        ".py",
    ),
    (
        "plain cargo at an MSVC target is fine — clippy does not link",
        "return ['cargo', *subcommand, '--target', 'x86_64-pc-windows-msvc']",
        ".py",
    ),
    # ---------------------------------------------------------------- #714 --
    # The widened scope reaches Rust and Dockerfiles, so their comment syntax
    # has to be understood or every doc comment becomes a violation.
    (
        "a Rust line comment describing the forbidden command",
        '    // cargo-zigbuild links via zig; never `xwin splat` here',
        ".rs",
    ),
    (
        "a Dockerfile comment describing the ban",
        "# do not `cargo install cargo-xwin` in this image — use soldr",
        "",
    ),
    (
        "a PowerShell comment describing the ban",
        "# cargo xwin is banned; soldr prepare provisions the CRT",
        ".ps1",
    ),
    # Words that merely contain a banned token are not invocations.
    ("crossbeam is not the cross wrapper", "use crossbeam::channel;", ".rs"),
    ("a variable named cross_build", "let cross_build_id = 3;", ".rs"),
    ("the ziglang test dependency pin", '    "ziglang>=0.15.2,<0.16",', ".toml"),
    (
        "an install-action for an unrelated tool",
        "      - uses: taiki-e/install-action@v2\n        with:\n          tool: cargo-nextest",
        ".yml",
    ),
    # The escape hatch, for prose no comment-stripper can see.
    (
        "a line carrying the documented allow marker",
        "legacy passthrough: cargo xwin build  (cross-lint: allow)",
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


def test_the_install_action_violation_points_at_the_action_line():
    """The multi-line install patterns match across lines, so the reported line
    number comes from an offset rather than an enumerate() counter. A finding
    that pointed at line 1 of a 400-line workflow would be useless."""
    text = "\n".join(
        [
            "jobs:",
            "  build:",
            "    steps:",
            "      - uses: taiki-e/install-action@v2",
            "        with:",
            "          tool: cargo-xwin",
        ]
    )
    (number, tool, reason) = scan_text(text, ".yml")[0]
    assert number == 4, "must point at the install-action step, not the file head"
    assert tool == "taiki-e/install-action: cargo-xwin"
    assert "soldr prepare" in reason


def test_the_install_window_does_not_pair_distant_lines():
    """An unbounded match would pair an install-action step at the top of a
    workflow with an unrelated `cargo-xwin` mention hundreds of lines below,
    which reads as a false accusation against whoever wrote the step."""
    text = "\n".join(
        ["      - uses: taiki-e/install-action@v2", *["        # filler"] * 10, "tool: cargo-xwin"]
    )
    tools = {tool for _, tool, _ in scan_text(text, ".yml")}
    assert "taiki-e/install-action: cargo-xwin" not in tools


def test_overlapping_rules_report_one_finding_per_line():
    """`cargo install cargo-xwin` matches both the install rule and the
    always-banned invocation rule. Reporting it twice would make the summary
    count lie about how much work a fix is."""
    findings = scan_text("cargo install cargo-xwin --locked", ".sh")
    assert findings, "must still be rejected"
    assert len(findings) == len(set(findings)), f"duplicate findings: {findings}"


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
