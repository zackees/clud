"""The review range is pinned to SHAs and ignores a stale tracking ref (#1301)."""

from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path

import pytest

from tests import process

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "git" / "review_range.py"
SKILL = ROOT / "crates" / "clud-bin" / "assets" / "skills" / "clud-review" / "SKILL.md"


@pytest.fixture
def rr():
    name = "clud_test_review_range"
    spec = importlib.util.spec_from_file_location(name, SCRIPT)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    try:
        yield module
    finally:
        sys.modules.pop(name, None)


def _git(cwd: Path, *args: str) -> str:
    result = process.run(
        [
            "git",
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@localhost",
            "-c",
            "commit.gpgsign=false",
            *args,
        ],
        cwd=str(cwd),
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert result.returncode == 0, (args, result.stderr)
    return result.stdout.strip()


def _commit(cwd: Path, name: str, body: str = "x\n") -> None:
    (cwd / name).write_text(body, encoding="utf-8")
    _git(cwd, "add", name)
    _git(cwd, "commit", "-q", "-m", f"add {name}")


@pytest.fixture
def rebased_branch(tmp_path: Path):
    """A branch pushed, then rebased onto a much newer main: its tracking ref is stale."""
    origin = tmp_path / "origin.git"
    origin.mkdir()
    _git(origin, "init", "-q", "--bare", "-b", "main")
    work = tmp_path / "work"
    _git(tmp_path, "clone", "-q", str(origin), "work")
    _git(work, "checkout", "-q", "-b", "main")
    _commit(work, "base.txt")
    _git(work, "push", "-q", "-u", "origin", "main")
    _git(work, "checkout", "-q", "-b", "feat")
    _commit(work, "feature.txt", "one\ntwo\n")
    _git(work, "push", "-q", "-u", "origin", "feat")
    # main moves on by many files, then the branch is rebased onto it.
    _git(work, "checkout", "-q", "main")
    for i in range(60):
        _commit(work, f"upstream-{i}.txt")
    _git(work, "push", "-q", "origin", "main")
    _git(work, "checkout", "-q", "feat")
    _git(work, "rebase", "-q", "main")
    return work


def test_resolver_ignores_the_stale_tracking_ref(rr, rebased_branch, monkeypatch) -> None:
    monkeypatch.chdir(rebased_branch)
    # The trap: the tracking ref is stale, so its three-dot range swallows the rebase delta.
    stale = _git(rebased_branch, "diff", "--numstat", "@{upstream}...HEAD")
    assert len(stale.splitlines()) > 50
    out = rr.resolve(fetch=False)
    assert out["source"] == "local"
    assert out["files"] == 1
    assert out["insertions"] == 2
    assert out["oversize"] is False
    assert out["range"] == f"{out['merge_base']}...{out['head']}"
    assert out["head"] == _git(rebased_branch, "rev-parse", "HEAD")
    assert out["merge_base"] == _git(rebased_branch, "rev-parse", "main")


def test_oversize_range_that_is_not_the_prs_own_diffstat_is_flagged(
    rr, rebased_branch, monkeypatch
):
    monkeypatch.chdir(rebased_branch)
    # Force a wrong, huge range: the original main (before the 60 commits) is not
    # what HEAD sits on, so diff HEAD against the root commit.
    root = _git(rebased_branch, "rev-list", "--max-parents=0", "HEAD")
    out = rr.resolve(base=root, fetch=False, max_files=50)
    assert out["merge_base"] == root
    assert out["files"] > 50
    assert out["oversize"] is True


def test_cli_exits_3_for_oversize_and_prints_json(rr, rebased_branch, monkeypatch, capsys) -> None:
    monkeypatch.chdir(rebased_branch)
    root = _git(rebased_branch, "rev-list", "--max-parents=0", "HEAD")
    code = rr.main(["--base", root, "--no-fetch"])
    assert code == rr.EXIT_OVERSIZE
    assert json.loads(capsys.readouterr().out)["oversize"] is True
    assert rr.main(["--no-fetch", "--bogus"]) == rr.EXIT_USAGE


def test_clud_review_diffs_only_through_the_resolver() -> None:
    text = " ".join(SKILL.read_text(encoding="utf-8").split())
    assert '"$CLUD_EXE" tool run git/review_range.py' in text
    assert "Never use `@{upstream}` or `@{u}`." in text
    assert "If it exits `3`" in text
