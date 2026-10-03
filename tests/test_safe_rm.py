"""Process-level behavior of the recoverable agent deletion command."""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

import pytest

from tests import process


def _run(
    tmp_path: Path,
    *args: str,
    session_roots: bool = True,
    cwd: Path | None = None,
    unsafe_mode: bool = False,
):
    binary = Path(os.environ["CLUD_TEST_BINARY"])
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    env = os.environ.copy()
    env.update(HOME=str(home), USERPROFILE=str(home))
    env.pop("CLUD_RM_ROOTS", None)
    if session_roots:
        env["CLUD_RM_ROOTS"] = str(tmp_path / "work")
    if unsafe_mode:
        env["CLUD_UNSAFE_MODE"] = "1"
    return process.run(
        [str(binary), "safe-rm", *args],
        cwd=cwd or (tmp_path if session_roots else tmp_path / "work"),
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )


def test_missing_operand_matches_rm_force_semantics(tmp_path: Path) -> None:
    work = tmp_path / "work"
    work.mkdir()
    missing = _run(tmp_path)
    assert missing.returncode == 1, missing
    assert "missing operand" in missing.stderr
    forced = _run(tmp_path, "-f")
    assert forced.returncode == 0, forced


def test_explicit_safe_rm_keeps_its_roots_in_unsafe_mode(tmp_path: Path) -> None:
    work = tmp_path / "work"
    work.mkdir()
    outside = tmp_path / "outside"
    outside.write_text("keep", encoding="utf-8")
    refused = _run(tmp_path, str(outside), unsafe_mode=True)
    assert refused.returncode != 0, refused
    assert outside.read_text(encoding="utf-8") == "keep"

    inside = work / "remove"
    inside.write_text("trash", encoding="utf-8")
    accepted = _run(tmp_path, str(inside), unsafe_mode=True)
    assert accepted.returncode == 0, accepted
    assert not inside.exists()
    assert any((tmp_path / "home" / ".clud" / "trash").iterdir())


@pytest.mark.parametrize("flags", [(), ("-r",), ("-rf",), ("-fr",), ("-R",), ("--recursive",)])
def test_trash_keeps_file_and_directory_structure(tmp_path: Path, flags: tuple[str, ...]) -> None:
    work = tmp_path / "work"
    work.mkdir()
    target = work / "nested" / "target"
    target.parent.mkdir()
    if flags:
        target.mkdir()
        (target / "note").write_text("keep", encoding="utf-8")
    else:
        target.write_text("keep", encoding="utf-8")
    result = _run(tmp_path, *flags, str(target))
    assert result.returncode == 0, result
    assert not target.exists()
    entries = list((tmp_path / "home" / ".clud" / "trash").iterdir())
    assert len(entries) == 1
    assert (entries[0] / ".clud-rm.json").is_file()
    moved = list(entries[0].glob("*/nested/target"))
    assert len(moved) == 1, entries
    content = moved[0] / "note" if flags else moved[0]
    assert content.read_text(encoding="utf-8") == "keep"


def test_audit_records_one_call_and_user_role(tmp_path: Path) -> None:
    work = tmp_path / "work"
    work.mkdir()
    target = work / "target"
    target.write_text("x", encoding="utf-8")
    assert _run(tmp_path, str(target), session_roots=False).returncode == 0
    audit = tmp_path / "home" / ".clud" / "state" / "logs" / ("r" + "m")
    records = []
    for path in audit.glob("*.jsonl"):
        for line in path.read_text(encoding="utf-8").splitlines():
            records.append(json.loads(line))
    assert len(records) == 1
    assert records[0]["role"] == "user"


def test_directory_requires_recursive_or_d_and_d_requires_empty(tmp_path: Path) -> None:
    work = tmp_path / "work"
    work.mkdir()
    directory = work / "directory"
    directory.mkdir()
    (directory / "note").write_text("x", encoding="utf-8")
    plain = _run(tmp_path, str(directory))
    assert plain.returncode == 1, plain
    assert "directory" in plain.stderr.lower()
    nonempty = _run(tmp_path, "-d", str(directory))
    assert nonempty.returncode == 1, nonempty
    (directory / "note").unlink()
    empty = _run(tmp_path, "-d", str(directory))
    assert empty.returncode == 0, empty
    assert not directory.exists()


def test_verbose_dry_run_and_purge(tmp_path: Path) -> None:
    work = tmp_path / "work"
    work.mkdir()
    target = work / "target"
    target.write_text("x", encoding="utf-8")
    planned = _run(tmp_path, "--dry-run", str(target))
    assert planned.returncode == 0, planned
    assert "would-trash" in planned.stdout
    assert target.exists()
    verbose = _run(tmp_path, "-v", str(target))
    assert verbose.returncode == 0, verbose
    assert f"removed '{target}'" in verbose.stdout
    target.write_text("again", encoding="utf-8")
    purged = _run(tmp_path, "--purge", str(target))
    assert purged.returncode == 0, purged
    assert not target.exists()


def test_tracked_path_needs_explicit_opt_in(tmp_path: Path) -> None:
    work = tmp_path / "work"
    work.mkdir()
    target = work / "tracked.txt"
    target.write_text("x", encoding="utf-8")
    init = process.run(["git", "init", "-q", str(work)], capture_output=True, text=True)
    assert init.returncode == 0, init
    added = process.run(
        ["git", "-C", str(work), "add", "tracked.txt"], capture_output=True, text=True
    )
    assert added.returncode == 0, added
    refused = _run(tmp_path, str(target))
    assert refused.returncode == 1, refused
    assert "git" in refused.stderr.lower()
    assert target.exists()
    permitted = _run(tmp_path, "--tracked", str(target))
    assert permitted.returncode == 0, permitted
    assert not target.exists()


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX symlink fixture")
def test_root_home_git_metadata_and_escaping_parent_are_refused(tmp_path: Path) -> None:
    work = tmp_path / "work"
    work.mkdir()
    home = tmp_path / "home"
    home.mkdir()
    outside = tmp_path / "outside"
    outside.mkdir()
    (outside / "keep").write_text("x", encoding="utf-8")
    metadata = work / ".git"
    metadata.mkdir()
    (metadata / "HEAD").write_text("ref: refs/heads/main", encoding="utf-8")
    (work / "escape").symlink_to(outside, target_is_directory=True)
    operands = [str(work), str(home), "/", str(metadata / "HEAD")]
    operands.append(str(work / "escape" / "keep"))
    for operand in operands:
        result = _run(tmp_path, "-rf", operand)
        assert result.returncode == 1, (operand, result)
    assert (outside / "keep").exists()
    assert (metadata / "HEAD").exists()


def test_mixed_batch_keeps_allowed_work_and_reports_refusal(tmp_path: Path) -> None:
    work = tmp_path / "work"
    work.mkdir()
    allowed = work / "allowed"
    allowed.write_text("x", encoding="utf-8")
    outside = tmp_path / "outside"
    outside.write_text("keep", encoding="utf-8")
    result = _run(tmp_path, str(outside), str(allowed))
    assert result.returncode == 1, result
    assert "outside" in result.stderr
    assert outside.read_text(encoding="utf-8") == "keep"
    assert not allowed.exists()


def test_options_stop_at_first_path_and_double_dash_accepts_dash_name(tmp_path: Path) -> None:
    work = tmp_path / "work"
    work.mkdir()
    for name in ["first", "--purge", "-dash"]:
        (work / name).write_text(name, encoding="utf-8")
    first = _run(tmp_path, "first", "--purge", cwd=work)
    assert first.returncode == 0, first
    assert not (work / "first").exists()
    assert not (work / "--purge").exists()
    assert len(list((tmp_path / "home" / ".clud" / "trash").iterdir())) == 1
    dashed = _run(tmp_path, "--", "-dash", cwd=work)
    assert dashed.returncode == 0, dashed
    assert not (work / "-dash").exists()


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX find/xargs fixture")
def test_find_exec_and_xargs_batches_use_the_safe_alias(tmp_path: Path) -> None:
    work = tmp_path / "work"
    work.mkdir()
    home = tmp_path / "home"
    home.mkdir()
    alias_dir = tmp_path / "bin"
    alias_dir.mkdir()
    binary = Path(os.environ["CLUD_TEST_BINARY"])
    (alias_dir / "safe-rm").symlink_to(binary.with_name("clud-shim"))
    env = os.environ.copy()
    env.update(
        HOME=str(home),
        USERPROFILE=str(home),
        CLUD_RM_ROOTS=str(work),
        PATH=str(alias_dir) + os.pathsep + env["PATH"],
    )
    for name in ["one.tmp", "two.tmp"]:
        (work / name).write_text(name, encoding="utf-8")
    found = process.run(
        ["find", str(work), "-type", "f", "-name", "*.tmp", "-exec", "safe-rm", "{}", "+"],
        cwd=work,
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert found.returncode == 0, found
    assert not list(work.glob("*.tmp"))
    paths = [work / "three.tmp", work / "four.tmp"]
    for path in paths:
        path.write_text("x", encoding="utf-8")
    listed = process.run(
        ["xargs", "-0", "safe-rm"],
        input="\0".join(map(str, paths)) + "\0",
        cwd=work,
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert listed.returncode == 0, listed
    assert all(not path.exists() for path in paths)
