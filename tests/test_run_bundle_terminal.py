"""Unit tests for how `ci/run_bundle.py` runs the PTY harness (#691).

The PTY tests used to skip silently whenever the harness's stdout was a pipe,
which is how CI ran every harness, so Windows had no coverage of clud under a
real console. Each `pty::` test now runs inside a pseudo-terminal with
`CLUD_REQUIRE_PTY=1`, which turns a canary failure into a red test (which
tests: `tests/test_harness_plan.py`).
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

from ci.run_bundle import REQUIRE_PTY_ENV, run_in_terminal


def test_require_pty_env_matches_the_rust_harness() -> None:
    root = Path(__file__).resolve().parents[1]
    common = root / "crates/clud-bin/tests/integration/common/mod.rs"
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
