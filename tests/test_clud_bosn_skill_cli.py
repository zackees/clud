"""The shipped Bosn skill describes the supported Rust CLI."""

import os
import re
import shutil
from pathlib import Path

import pytest

from tests import process

ROOT = Path(__file__).resolve().parents[1]
SKILL = ROOT / "crates/clud-bin/assets/skills/clud-bosn/SKILL.md"


def test_skill_uses_current_manifest_commands():
    text = SKILL.read_text(encoding="utf-8")
    assert "bosn run --stack" in text
    assert "bosn run --task" in text
    assert "bosn daemon status --state-dir" in text
    assert "bosn gc owned --state-dir" in text
    for old in ("bosn ensure", "bosn unit", "bosn status --json", "bosn gc --dry-run"):
        assert old not in text


def test_documented_command_families_exist_in_installed_bosn():
    bosn = os.environ.get("BOSN_TEST_EXE") or shutil.which("bosn")
    if bosn is None:
        pytest.skip("bosn is not installed")
    version = process.run(
        [bosn, "--version"], capture_output=True, text=True, check=True, timeout=10
    )
    match = re.search(r"\b(\d+)\.(\d+)\.(\d+)\b", version.stdout + version.stderr)
    assert match is not None, "bosn --version did not report a version"
    if tuple(int(part) for part in match.groups()) < (0, 1, 29):
        pytest.skip("installed bosn predates the supported 0.1.29 CLI")
    result = process.run(
        [bosn, "--help"], capture_output=True, text=True, check=False, timeout=10
    )
    usage = result.stdout + result.stderr
    for command in (
        "bosn run --task",
        "bosn run --stack",
        "bosn daemon status --state-dir",
        "bosn gc owned --state-dir",
    ):
        assert command in usage
