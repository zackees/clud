"""`bash lint --windows` also type-checks the Windows target (#1441)."""

from __future__ import annotations

from ci import lint


def test_host_clippy_only_by_default() -> None:
    assert lint.clippy_subcommands(False) == [
        ["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]
    ]


def test_windows_flag_adds_the_msvc_target_after_the_host() -> None:
    host, windows = lint.clippy_subcommands(True)
    assert "--target" not in host
    assert windows[:4] == ["clippy", "--workspace", "--all-targets", "--target"]
    assert windows[4] == "x86_64-pc-windows-msvc"
    # Warnings are errors on both, exactly as in CI.
    assert windows[-3:] == ["--", "-D", "warnings"]
