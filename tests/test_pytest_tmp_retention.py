"""A passing run leaves no tmp_path worlds; a failing one keeps and names them (#1686).

Harness tests build a repo, a bare origin and a Claude config per test under
`tmp_path`. pytest keeps the last three runs' base temps by default, each a
full set of worlds, under `$TMPDIR` (`~/.clud/tmp` in a clud session).
"""

from __future__ import annotations

import os
import stat
from pathlib import Path

import pytest

pytest_plugins = ("pytester",)

_REPO_ROOT = Path(__file__).resolve().parents[1]

# A world with a read-only directory, like git's object store: pytest's own
# `rmtree(ignore_errors=True)` cannot remove it on any platform.
_WORLD = """
import os, stat

def _world(tmp_path):
    objects = tmp_path / "objects"
    objects.mkdir()
    (objects / "blob").write_text("x")
    os.chmod(objects / "blob", stat.S_IREAD)
    os.chmod(objects, stat.S_IREAD | stat.S_IEXEC)
"""


def _run(pytester: pytest.Pytester, body: str) -> tuple[int, list[Path]]:
    pytester.makeini("[pytest]\ntmp_path_retention_policy = failed\n")
    pytester.makepyfile(_WORLD + body)
    # inline_run, unlike runpytest, injects no --basetemp, so the inner session
    # takes the default base temp under pytester's private temp root.
    reprec = pytester.inline_run("-p", "ci.pytest_tmp_retention", "-p", "no:cacheprovider")
    roots = list(Path(os.environ["PYTEST_DEBUG_TEMPROOT"]).glob("pytest-of-*/pytest-*"))
    return reprec.ret, roots


def test_pyproject_drops_passing_tests_tmp_paths() -> None:
    text = (_REPO_ROOT / "pyproject.toml").read_text(encoding="utf-8")
    assert '\ntmp_path_retention_policy = "failed"\n' in text


def test_repo_conftest_loads_the_retention_plugin() -> None:
    from tests import conftest

    assert conftest.pytest_sessionfinish.__module__ == "ci.pytest_tmp_retention"


def test_passing_run_removes_read_only_worlds(pytester: pytest.Pytester) -> None:
    ret, roots = _run(pytester, "\ndef test_ok(tmp_path):\n    _world(tmp_path)\n")
    assert ret == 0
    assert roots == [], f"passing run left worlds behind: {roots}"


def test_failing_run_keeps_worlds_and_prints_path(
    pytester: pytest.Pytester, capsys: pytest.CaptureFixture[str]
) -> None:
    ret, roots = _run(pytester, "\ndef test_bad(tmp_path):\n    _world(tmp_path)\n    assert False\n")
    out = capsys.readouterr().out
    assert ret == 1
    assert len(roots) == 1
    assert list(roots[0].glob("test_bad*/objects/blob"))
    assert f"pytest tmp_path worlds kept for the failed run: {roots[0]}" in out
    # Let pytester clean up its own temp root.
    for path in roots[0].rglob("*"):
        os.chmod(path, stat.S_IWRITE | stat.S_IREAD | stat.S_IEXEC)
