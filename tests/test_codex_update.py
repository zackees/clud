"""The trusted updater is a real CLI command, not backend passthrough."""

from __future__ import annotations

import os
import sys
from pathlib import Path

from tests import process


def clud_binary() -> Path:
    suffix = ".exe" if sys.platform == "win32" else ""
    binary = Path(os.environ.get("CLUD_TEST_BINARY", "target/debug/clud"))
    if suffix and binary.suffix != suffix:
        binary = binary.with_suffix(suffix)
    return binary


def test_codex_update_help_is_dispatched_without_backend() -> None:
    result = process.run(
        [str(clud_binary()), "codex-update", "--help"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 0, result
    assert "Install or update Codex" in result.stdout


def test_codex_update_rejects_passthrough_without_fetching() -> None:
    result = process.run(
        [str(clud_binary()), "codex-update", "--", "--script", "untrusted.sh"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 2, result
    assert "accepts no passthrough" in result.stderr
