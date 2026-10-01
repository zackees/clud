"""Bundled tools keep stdout bounded (issue #1676).

A tool whose result can exceed ~32 KB prints the first 32 KB, an explicit
truncation notice, and the path of an artifact holding the full output under
the clud tmp dir. Small output stays complete and byte-identical, and a failed
artifact write still truncates visibly instead of crashing.
"""

from __future__ import annotations

import importlib.util
import re
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
TOOLS = ROOT / "crates" / "clud-bin" / "assets" / "tools"
CAPPED = {
    "transcript_report": TOOLS / "diagnostics" / "transcript_report.py",
    "lint_deadcode": TOOLS / "python" / "lint_deadcode.py",
}


def _load(name: str, path: Path):
    mod_name = f"clud_test_bounded_{name}"
    spec = importlib.util.spec_from_file_location(mod_name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[mod_name] = module
    spec.loader.exec_module(module)
    return module


@pytest.fixture(autouse=True)
def _temp_home(tmp_path, monkeypatch):
    home = tmp_path / "home"
    home.mkdir()
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("USERPROFILE", str(home))
    monkeypatch.delenv("CLUD_TOOL_OUTPUT_DIR", raising=False)
    return home


@pytest.fixture(params=sorted(CAPPED))
def tool(request):
    return _load(request.param, CAPPED[request.param])


def test_small_output_is_complete_and_byte_identical(tool, capsys, _temp_home):
    text = "line one\nline two ünïcode\n" * 10
    tool.emit_bounded(text, "probe")
    assert capsys.readouterr().out == text + "\n"
    assert not (_temp_home / ".clud").exists()


def test_oversized_output_truncates_with_notice_and_artifact(tool, capsys, _temp_home):
    text = "".join(f"row {i:06d} {'x' * 60}\n" for i in range(2000))
    assert len(text.encode()) > 32 * 1024
    tool.emit_bounded(text, "probe")
    out = capsys.readouterr().out
    assert len(out.encode()) < 33 * 1024
    assert out.startswith(text[:1000])
    match = re.search(r"\[TRUNCATED: .* full output: (.+)\]", out)
    assert match, out[-400:]
    artifact = Path(match.group(1).strip())
    assert artifact.is_file()
    assert artifact.read_text(encoding="utf-8") == text
    assert (_temp_home / ".clud" / "tmp") in artifact.parents


def test_artifact_write_failure_still_truncates(tool, capsys, tmp_path, monkeypatch):
    blocker = tmp_path / "not-a-dir"
    blocker.write_text("file in the way", encoding="utf-8")
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(blocker / "sub"))
    text = "y" * (64 * 1024)
    tool.emit_bounded(text, "probe")
    out = capsys.readouterr().out
    assert "[TRUNCATED:" in out
    assert "could not be saved" in out
    assert len(out.encode()) < 33 * 1024


def test_pr_merge_watch_caps_a_giant_first_error_line():
    pmw = _load("pr_merge_watch", TOOLS / "github" / "pr_merge_watch.py")
    check = pmw.CheckRow(name="unit", state="FAILURE", bucket="fail", link="")
    giant = "error: " + "z" * 50_000
    rendered = pmw.FailureReport(check, "123", giant, None).render()
    assert len(rendered) < 4096
    assert "[TRUNCATED:" in rendered
    short = pmw.FailureReport(check, "123", "error: boom", None).render()
    assert "  first error: error: boom" in short.splitlines()
