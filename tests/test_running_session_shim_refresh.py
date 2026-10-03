"""An upgraded `clud` refreshes a running session's shared aliases (#1743).

A session keeps the alias directory it launched with (`~/.clud/state/rm-shim`),
so after an upgrade its `gh`, `git` and `rm` still run the old binary's code
until something relinks them. The session's own statusline, hooks and `clud
tool` calls run the installed `clud` by path, and any of them relinks the
directory when its aliases are older than that `clud`. See
docs/architecture/shim-dispatch.md ("Running sessions after an upgrade").
"""

from __future__ import annotations

import os
import shutil
import sys
from pathlib import Path

import pytest

from tests import process
from tests.shim_env import registry

pytestmark = pytest.mark.skipif(sys.platform == "win32", reason="POSIX inode checks")


def _binary(name: str) -> Path:
    clud = os.environ.get("CLUD_TEST_BINARY")
    candidate = (
        Path(clud).with_name(name)
        if clud
        else Path(__file__).resolve().parents[1] / "target" / "debug" / name
    )
    assert candidate.is_file(), candidate
    return candidate


def _old_aliases(home: Path) -> Path:
    """The alias dir an older clud left behind: every name one older file."""
    alias_dir = home / ".clud" / "state" / "rm-shim"
    alias_dir.mkdir(parents=True)
    old = alias_dir / ".old-clud"
    old.write_bytes(b"older clud")
    old.chmod(0o755)
    # Older than any build of the binary under test, however stale its mtime.
    os.utime(old, (1_000_000_000, 1_000_000_000))
    for name in ("gh", "git", "rm", "safe-rm", "safe-mktemp"):
        os.link(old, alias_dir / name)
    old.unlink()
    return alias_dir


def _session_env(home: Path, clud: Path, alias_dir: Path) -> dict[str, str]:
    info = registry(_binary("clud-shim"))
    env = os.environ.copy()
    env.update(
        {
            "HOME": str(home),
            "CLUD_EXE": str(clud),
            info["abi_key"]: info["abi"],
            info["session_dir_key"]: str(alias_dir),
        }
    )
    return env


def test_the_installed_clud_relinks_a_running_sessions_aliases(tmp_path: Path) -> None:
    clud = _binary("clud")
    home = tmp_path / "home"
    alias_dir = _old_aliases(home)
    result = process.run(
        [str(clud), "--version"],
        env=_session_env(home, clud, alias_dir),
        capture_output=True,
        timeout=60,
    )
    assert result.returncode == 0, result
    for name in ("gh", "git", "rm", "safe-rm", "safe-mktemp"):
        assert os.path.samefile(alias_dir / name, clud), name


def test_another_clud_leaves_the_session_aliases_alone(tmp_path: Path) -> None:
    """Only the session's own installed `clud` refreshes: a dev build run by
    hand (`CLUD_EXE` names another binary) never takes over the shared dir."""
    clud = _binary("clud")
    home = tmp_path / "home"
    alias_dir = _old_aliases(home)
    env = _session_env(home, clud, alias_dir)
    other = tmp_path / "other" / "clud"
    other.parent.mkdir()
    shutil.copy2(clud, other)
    env["CLUD_EXE"] = str(other)
    result = process.run([str(clud), "--version"], env=env, capture_output=True, timeout=60)
    assert result.returncode == 0, result
    assert (alias_dir / "gh").read_bytes() == b"older clud"
