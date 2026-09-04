"""Tests for the read-only-artifact explanation in `ci.lint` (#1158)."""

from __future__ import annotations

import os
import stat
from pathlib import Path

import pytest

from ci import lint


def _make_deps(tmp_path: Path) -> Path:
    deps = tmp_path / "target" / "debug" / "deps"
    deps.mkdir(parents=True)
    return deps


def _read_only(path: Path) -> None:
    path.write_bytes(b"artifact")
    path.chmod(stat.S_IRUSR | stat.S_IRGRP | stat.S_IROTH)


READONLY_UNENFORCED = pytest.mark.skipif(
    os.name == "nt",
    reason="chmod read-only is not enforced for the owner on Windows",
)


def test_no_target_dir_says_nothing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(lint, "ROOT", tmp_path)
    lint.explain_readonly_failure(["cargo", "fmt"])
    assert capsys.readouterr().err == ""


def test_a_writable_tree_says_nothing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    """A tree the plain driver built itself has nothing read-only in it, so
    the failure above was about something else and this must stay quiet."""
    deps = _make_deps(tmp_path)
    (deps / "libfoo.rmeta").write_bytes(b"artifact")
    monkeypatch.setattr(lint, "ROOT", tmp_path)
    lint.explain_readonly_failure(["cargo", "fmt"])
    assert capsys.readouterr().err == ""


@READONLY_UNENFORCED
def test_a_readonly_artifact_with_a_plain_driver_is_explained(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    deps = _make_deps(tmp_path)
    _read_only(deps / "libserde_core-abc123.rmeta")
    monkeypatch.setattr(lint, "ROOT", tmp_path)

    lint.explain_readonly_failure(["/usr/bin/cargo", "fmt"])

    err = capsys.readouterr().err
    assert "read-only" in err
    assert "soldr" in err, "the message must name the fix, not just the symptom"


@READONLY_UNENFORCED
def test_a_readonly_artifact_under_soldr_says_nothing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    """soldr reads its own cache-linked tree happily, so a failure there is
    about something else and this explanation would be a red herring."""
    deps = _make_deps(tmp_path)
    _read_only(deps / "libserde_core-abc123.rmeta")
    monkeypatch.setattr(lint, "ROOT", tmp_path)

    lint.explain_readonly_failure(["/home/u/.soldr/bin/soldr", "cargo", "fmt"])

    assert capsys.readouterr().err == ""


@READONLY_UNENFORCED
def test_it_returns_none_and_never_changes_an_exit_code(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The load-bearing property. An earlier draft gated on this condition and
    returned non-zero, which blocked lint runs that would have passed --
    `soldr cargo build` repopulates the read-only links on every build, while
    `fmt` does not compile and succeeds against them anyway."""
    deps = _make_deps(tmp_path)
    _read_only(deps / "libserde_core-abc123.rmeta")
    monkeypatch.setattr(lint, "ROOT", tmp_path)

    assert lint.explain_readonly_failure(["/usr/bin/cargo", "fmt"]) is None


def test_the_probe_is_bounded(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    """It runs on every failing lint, so it must not walk an unbounded tree."""
    deps = _make_deps(tmp_path)
    for index in range(10):
        (deps / f"lib{index}.rmeta").write_bytes(b"artifact")
    monkeypatch.setattr(lint, "ROOT", tmp_path)
    monkeypatch.setattr(lint, "_READONLY_PROBE_LIMIT", 3)

    lint.explain_readonly_failure(["cargo", "fmt"])

    assert capsys.readouterr().err == ""
