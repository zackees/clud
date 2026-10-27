"""Unit coverage for the bundled `code/patch.py` tool (#1339 item 5)."""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

TOOL_DIR = Path(__file__).resolve().parents[1] / "crates" / "clud-bin" / "assets" / "tools" / "code"


def _load():
    if str(TOOL_DIR) not in sys.path:
        sys.path.insert(0, str(TOOL_DIR))
    import patch

    return patch


patch = _load()


def _block(path: str, search: str, replace: str) -> str:
    return f"=== {path}\n<<<<<<< SEARCH\n{search}=======\n{replace}>>>>>>> REPLACE\n"


def test_applies_blocks_across_files_in_order(tmp_path: Path) -> None:
    (tmp_path / "a.rs").write_bytes(b"fn a() {}\nfn b() {}\n")
    text = (
        _block("a.rs", "fn a() {}\n", "fn a2() {}\n")
        + _block("a.rs", "fn a2() {}\n", "fn a3() {}\n")
        + _block("new/b.py", "", "x = 1\n")
    )
    result = patch.plan(patch.parse(text), tmp_path, None)
    assert result[(tmp_path / "a.rs").resolve()] == "fn a3() {}\nfn b() {}\n"
    assert result[(tmp_path / "new/b.py").resolve()] == "x = 1\n"


@pytest.mark.parametrize(
    ("content", "found"),
    [("x\nx\n", "2 matches"), ("y\n", "0 matches")],
)
def test_a_search_must_match_exactly_once(tmp_path: Path, content: str, found: str) -> None:
    (tmp_path / "f.txt").write_bytes(content.encode("utf-8"))
    with pytest.raises(patch.PatchError, match=found) as err:
        patch.plan(patch.parse(_block("f.txt", "x\n", "z\n")), tmp_path, None)
    assert err.value.code == 2


def test_a_file_outside_the_task_refuses_the_whole_patch(tmp_path: Path) -> None:
    (tmp_path / "mine.rs").write_bytes(b"a\n")
    (tmp_path / "theirs.rs").write_bytes(b"a\n")
    facts = {"tasks": [{"worktree": str(tmp_path), "files": ["mine.rs"]}]}
    allowed = patch.task_files(facts, tmp_path.resolve())
    text = _block("mine.rs", "a\n", "b\n") + _block("theirs.rs", "a\n", "b\n")
    with pytest.raises(patch.PatchError, match=r"theirs\.rs: not in this task's files"):
        patch.plan(patch.parse(text), tmp_path, allowed)


def test_tasks_in_other_checkouts_do_not_restrict(tmp_path: Path) -> None:
    facts = {"repos": [{"tasks": [{"worktree": str(tmp_path / "other"), "files": ["x"]}]}]}
    assert patch.task_files(facts, tmp_path.resolve()) is None


def test_line_endings_are_kept(tmp_path: Path) -> None:
    (tmp_path / "w.txt").write_bytes(b"keep\r\nold\r\n")
    result = patch.plan(patch.parse(_block("w.txt", "old\r\n", "new\r\n")), tmp_path, None)
    assert result[(tmp_path / "w.txt").resolve()] == "keep\r\nnew\r\n"


def test_malformed_patches_are_usage_errors() -> None:
    for text in ["", "stray line\n", "<<<<<<< SEARCH\nx\n=======\ny\n>>>>>>> REPLACE\n"]:
        with pytest.raises(patch.PatchError) as err:
            patch.parse(text)
        assert err.value.code == 1
