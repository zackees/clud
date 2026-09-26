"""The tracked-file guard that fails a test run writing the checkout (#1426)."""

from __future__ import annotations

import shutil
from pathlib import Path

import pytest

from ci import tracked_files
from tests import process

pytestmark = pytest.mark.skipif(shutil.which("git") is None, reason="git not installed")


def _git(repo: Path, *args: str) -> None:
    process.run(
        [
            "git",
            "-c",
            "user.name=clud-test",
            "-c",
            "user.email=clud-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            *args,
        ],
        cwd=repo,
        capture_output=True,
        text=True,
        check=True,
    )


@pytest.fixture
def repo(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setenv("GIT_CEILING_DIRECTORIES", str(tmp_path))
    root = tmp_path / "repo"
    root.mkdir()
    _git(root, "init", "-q")
    (root / ".claude").mkdir()
    (root / ".claude" / "settings.json").write_text('{"command": "clud-cmd-scan"}\n')
    (root / "other.txt").write_text("one\n")
    _git(root, "add", ".")
    _git(root, "commit", "-q", "-m", "init")
    return root


def test_clean_checkout_that_stays_clean_reports_nothing(repo: Path) -> None:
    before = tracked_files.snapshot(repo)
    (repo / "untracked.txt").write_text("scratch\n")
    after = tracked_files.snapshot(repo)
    assert before == {}
    assert after is not None
    assert tracked_files.changed_paths(before, after) == []


def test_a_rewritten_tracked_file_is_named(repo: Path) -> None:
    before = tracked_files.snapshot(repo)
    (repo / ".claude" / "settings.json").write_text('{"command": "/abs/clud-cmd-scan"}\n')
    after = tracked_files.snapshot(repo)
    assert before is not None
    assert after is not None
    assert tracked_files.changed_paths(before, after) == [".claude/settings.json"]
    message = tracked_files.describe([".claude/settings.json"], during="the pytest run")
    assert ".claude/settings.json" in message
    assert "#1426" in message


def test_a_checkout_dirty_before_the_run_does_not_fail(repo: Path) -> None:
    (repo / "other.txt").write_text("edited by the developer\n")
    before = tracked_files.snapshot(repo)
    after = tracked_files.snapshot(repo)
    assert before is not None
    assert after is not None
    assert "other.txt" in before
    assert tracked_files.changed_paths(before, after) == []


def test_a_further_edit_to_an_already_dirty_file_is_caught(repo: Path) -> None:
    (repo / "other.txt").write_text("edited by the developer\n")
    before = tracked_files.snapshot(repo)
    (repo / "other.txt").write_text("edited again by a test\n")
    after = tracked_files.snapshot(repo)
    assert before is not None
    assert after is not None
    assert tracked_files.changed_paths(before, after) == ["other.txt"]


def test_a_deleted_tracked_file_is_caught(repo: Path) -> None:
    before = tracked_files.snapshot(repo)
    (repo / "other.txt").unlink()
    after = tracked_files.snapshot(repo)
    assert before is not None
    assert after is not None
    assert tracked_files.changed_paths(before, after) == ["other.txt"]


def test_outside_a_git_checkout_the_guard_is_skipped(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("GIT_CEILING_DIRECTORIES", str(tmp_path))
    plain = tmp_path / "plain"
    plain.mkdir()
    assert tracked_files.snapshot(plain) is None
