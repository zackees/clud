"""Unit tests for how `ci/run_bundle.py` runs the PTY harness (#691).

The PTY tests used to skip silently whenever the harness's stdout was a pipe,
which is how CI ran every harness, so Windows had no coverage of clud under a
real console. The `pty` harness now runs inside a pseudo-terminal with
`CLUD_REQUIRE_PTY=1`, which turns a canary failure into a red test.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

from ci.run_bundle import REQUIRE_PTY_ENV, needs_terminal, run_in_terminal


@pytest.mark.parametrize(
    "name",
    ["pty-ad3bdf1f57219854", "pty-ad3bdf1f57219854.exe", "pty-0"],
)
def test_pty_harness_runs_in_a_terminal(name: str) -> None:
    assert needs_terminal(Path(name))


@pytest.mark.parametrize(
    "name",
    [
        "orphan_reap-1a2b3c",
        "cli-9f8e7d.exe",
        "pty_extra-1234",
        "empty-pty-1234",
        "pty",
    ],
)
def test_other_harnesses_keep_piped_stdio(name: str) -> None:
    assert not needs_terminal(Path(name))


def test_require_pty_env_matches_the_rust_harness() -> None:
    common = Path(__file__).resolve().parents[1] / "crates/clud-bin/tests/common/mod.rs"
    assert f'REQUIRE_PTY_ENV: &str = "{REQUIRE_PTY_ENV}"' in common.read_text(encoding="utf-8")


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX tty check")
def test_run_in_terminal_gives_the_child_a_tty_and_the_require_flag(
    capsys: pytest.CaptureFixture[str],
) -> None:
    script = (
        "import os, sys; "
        "print('tty', sys.stdin.isatty() and sys.stdout.isatty()); "
        f"print('flag', os.environ.get('{REQUIRE_PTY_ENV}')); "
        "sys.exit(3)"
    )
    rc = run_in_terminal([sys.executable, "-c", script], dict(os.environ))
    out = capsys.readouterr().out
    assert rc == 3
    assert "tty True" in out
    assert "flag 1" in out
