"""Verify the clud binary prints Hello, world! and exits 0."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path


def _clud_binary() -> str:
    """Find the clud binary in the venv."""
    if sys.platform == "win32":
        venv = Path(sys.executable).parent
        candidate = venv / "clud.exe"
    else:
        venv = Path(sys.executable).parent
        candidate = venv / "clud"
    if candidate.is_file():
        return str(candidate)
    return "clud"


def test_hello_world() -> None:
    result = subprocess.run(
        [_clud_binary()],
        capture_output=True,
        text=True,
        timeout=10,
    )
    assert result.returncode == 0
    assert "Hello, world!" in result.stdout
