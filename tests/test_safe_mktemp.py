"""Process-level behavior of `safe-mktemp`, the creation-ledger writer (#1667)."""

from __future__ import annotations

import os
import sys
from pathlib import Path

from tests import process


def _run(tmp_path: Path, *args: str, session: str | None = "session-1"):
    binary = Path(os.environ["CLUD_TEST_BINARY"])
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    env = os.environ.copy()
    env.update(
        HOME=str(home),
        USERPROFILE=str(home),
        CLUD_DAEMON_STATE_DIR=str(tmp_path / "state"),
    )
    for key in ("CLUD_SESSION_ID", "CLAUDE_CODE_SESSION_ID"):
        env.pop(key, None)
    if session is not None:
        env["CLUD_SESSION_ID"] = session
    return process.run(
        [str(binary), "__shim", "safe-mktemp", *args],
        cwd=str(tmp_path),
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )


def test_existing_path_fails_and_records_nothing(tmp_path: Path) -> None:
    existing = tmp_path / "existing"
    existing.mkdir()
    (existing / "keep.txt").write_text("keep", encoding="utf-8")
    result = _run(tmp_path, str(existing))
    assert result.returncode != 0, result
    assert result.stdout == ""
    if sys.platform != "win32":
        assert "already exists" in result.stderr
    assert (existing / "keep.txt").read_text(encoding="utf-8") == "keep"
    # The daemon was never asked: no state directory was created.
    assert not (tmp_path / "state").exists()


def test_parents_are_not_created(tmp_path: Path) -> None:
    result = _run(tmp_path, str(tmp_path / "a" / "b"))
    assert result.returncode != 0, result
    assert not (tmp_path / "a").exists()


def test_without_a_session_nothing_is_created(tmp_path: Path) -> None:
    result = _run(tmp_path, str(tmp_path / "scratch"), session=None)
    assert result.returncode == 2, result
    assert not (tmp_path / "scratch").exists()


def test_windows_refuses_plainly(tmp_path: Path) -> None:
    if sys.platform != "win32":
        return
    result = _run(tmp_path, str(tmp_path / "scratch"))
    assert result.returncode == 2, result
    assert "Windows" in result.stderr
    assert not (tmp_path / "scratch").exists()
