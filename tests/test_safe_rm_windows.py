"""Windows-native and MSYS spelling checks for the deletion command."""

from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

from tests import process

pytestmark = pytest.mark.skipif(sys.platform != "win32", reason="Windows-only filesystem cases")


def _run(tmp_path: Path, *args: str):
    binary = Path(os.environ["CLUD_TEST_BINARY"])
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    work = tmp_path / "work"
    work.mkdir(exist_ok=True)
    env = os.environ.copy()
    env.update(HOME=str(home), USERPROFILE=str(home), CLUD_RM_ROOTS=str(work))
    return process.run(
        [str(binary), "safe-rm", *args],
        cwd=work,
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )


def test_native_and_msys_drive_paths_both_trash_inside_root(tmp_path: Path) -> None:
    work = tmp_path / "work"
    work.mkdir()
    native = work / "native.txt"
    msys = work / "msys.txt"
    native.write_text("native", encoding="utf-8")
    msys.write_text("msys", encoding="utf-8")
    native_result = _run(tmp_path, str(native))
    assert native_result.returncode == 0, native_result
    drive = work.drive[0].lower()
    msys_spelling = f"/{drive}{msys.as_posix()[2:]}"
    msys_result = _run(tmp_path, msys_spelling)
    assert msys_result.returncode == 0, msys_result
    assert not native.exists()
    assert not msys.exists()


def test_recursive_target_containing_junction_is_refused(tmp_path: Path) -> None:
    work = tmp_path / "work"
    work.mkdir()
    parent = work / "parent"
    parent.mkdir()
    outside = tmp_path / "outside"
    outside.mkdir()
    sentinel = outside / "keep.txt"
    sentinel.write_text("keep", encoding="utf-8")
    junction = parent / "junction"
    created = process.run(
        ["cmd", "/d", "/c", f'mklink /J "{junction}" "{outside}"'],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert created.returncode == 0, created
    refused = _run(tmp_path, "-r", str(parent))
    assert refused.returncode == 1, refused
    assert "reparse" in refused.stderr.lower() or "junction" in refused.stderr.lower()
    assert sentinel.read_text(encoding="utf-8") == "keep"
    assert parent.exists()
