"""#2101: `clud gc keep` pins a path in ~/.clud/gc-keep.json without the daemon."""

import json
from pathlib import Path

from tests.test_hello import _run


def _env(home: Path) -> dict[str, str]:
    return {"HOME": str(home), "USERPROFILE": str(home)}


def test_gc_keep_adds_lists_and_removes_a_path(tmp_path: Path) -> None:
    home = tmp_path / "home"
    home.mkdir()
    target = tmp_path / "wt"
    target.mkdir()
    env = _env(home)
    added = _run("--no-daemon", "gc", "keep", str(target), env_overrides=env)
    assert added.returncode == 0, added.stderr
    assert "kept" in added.stdout
    keep_file = home / ".clud" / "gc-keep.json"
    paths = json.loads(keep_file.read_text(encoding="utf-8"))["paths"]
    assert paths == [str(target.resolve())]
    listed = _run("--no-daemon", "gc", "keep", "--list", env_overrides=env)
    assert listed.returncode == 0, listed.stderr
    assert str(target.resolve()) in listed.stdout
    removed = _run("--no-daemon", "gc", "keep", "--remove", str(target), env_overrides=env)
    assert removed.returncode == 0, removed.stderr
    assert json.loads(keep_file.read_text(encoding="utf-8"))["paths"] == []
