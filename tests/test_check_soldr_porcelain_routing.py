"""Routing of agent validation runs onto Soldr's bounded `--porcelain` mode.

forge-ci/soldr ships `--porcelain` / `SOLDR_PORCELAIN=1` (soldr#3924): ONE JSON
line on stdout plus a durable `log_path` instead of relaying every passing test
and per-crate cache record. clud#2231 is the consumer side, so the two surfaces
an agent actually reads have to name it:

* the `check-soldr` hook denial, which is where agents learn how to re-run a
  blocked `cargo` call, and
* the fix/review skills, whose "run the targeted test" lines are copied
  verbatim into agent commands.

The hint has to stay safe on an older Soldr (the variable is ignored there), so
these tests pin the fallback wording as well as the spelling.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

import pytest

from tests import process

ROOT = Path(__file__).resolve().parent.parent
HOOK = ROOT / ".codex" / "hooks" / "check-soldr.py"
FIX_SKILL = ROOT / "crates" / "clud-bin" / "assets" / "skills" / "clud-fix-quick" / "SKILL.md"
REVIEW_SKILL = ROOT / "crates" / "clud-bin" / "assets" / "skills" / "clud-review" / "SKILL.md"
PORCELAIN_ENV = "SOLDR_PORCELAIN=1"


def _path_with_soldr(tmp_path: Path) -> str:
    bindir = tmp_path / "bin"
    bindir.mkdir()
    stub = bindir / "soldr"
    stub.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    stub.chmod(0o755)
    return str(bindir)


def _run_hook(command: str, path: str) -> process.CompletedProcess[str]:
    payload = json.dumps(
        {
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "hook_event_name": "PreToolUse",
        }
    )
    env = os.environ.copy()
    env["PATH"] = path
    env["CLUD_HOOK_STDIN_IDLE_TIMEOUT_SEC"] = "0.05"
    env["CLUD_HOOK_STDIN_DEADLINE_SEC"] = "0.5"
    return process.run(
        [sys.executable, str(HOOK)],
        input=payload,
        capture_output=True,
        text=True,
        env=env,
        timeout=30,
        check=False,
    )


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX stub binary fixture")
def test_blocked_test_command_points_at_porcelain(tmp_path: Path) -> None:
    result = _run_hook("cargo test -p soldr-cli --lib porcelain", _path_with_soldr(tmp_path))
    assert result.returncode == 2, result.stderr
    assert "soldr cargo ..." in result.stderr
    assert PORCELAIN_ENV in result.stderr
    assert "log_path" in result.stderr
    # The fallback must be spelled out: older Soldr ignores the variable.
    assert "ignore the variable" in result.stderr


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX stub binary fixture")
def test_hint_survives_a_global_option_before_the_verb(tmp_path: Path) -> None:
    result = _run_hook("cargo --locked build --workspace", _path_with_soldr(tmp_path))
    assert result.returncode == 2, result.stderr
    assert PORCELAIN_ENV in result.stderr


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX stub binary fixture")
def test_non_validation_cargo_call_gets_no_porcelain_noise(tmp_path: Path) -> None:
    result = _run_hook("cargo fmt --all -- --check", _path_with_soldr(tmp_path))
    assert result.returncode == 2, result.stderr
    assert "Re-run as: soldr cargo" in result.stderr
    assert PORCELAIN_ENV not in result.stderr


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX stub binary fixture")
def test_rustc_denial_has_no_cargo_porcelain_hint(tmp_path: Path) -> None:
    result = _run_hook("rustc --version", _path_with_soldr(tmp_path))
    assert result.returncode == 2, result.stderr
    assert PORCELAIN_ENV not in result.stderr


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX stub binary fixture")
def test_missing_soldr_keeps_the_install_hint_only(tmp_path: Path) -> None:
    empty = tmp_path / "empty"
    empty.mkdir()
    result = _run_hook("cargo test -p clud-bin", str(empty))
    assert result.returncode == 2, result.stderr
    assert "is not on PATH" in result.stderr
    assert "Install it with" in result.stderr
    assert PORCELAIN_ENV not in result.stderr


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX stub binary fixture")
def test_routed_soldr_call_is_allowed_silently(tmp_path: Path) -> None:
    result = _run_hook(
        f"{PORCELAIN_ENV} soldr cargo test -p clud-bin --lib skills::",
        _path_with_soldr(tmp_path),
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == ""
    assert result.stderr == ""


@pytest.mark.parametrize("skill", [FIX_SKILL, REVIEW_SKILL], ids=lambda p: p.parent.name)
def test_skills_route_rust_tests_through_porcelain(skill: Path) -> None:
    text = skill.read_text(encoding="utf-8")
    assert PORCELAIN_ENV in text
    assert "log_path" in text
    # Do not turn the bounded mode into an unconditional default: the skill has
    # to describe the pre-porcelain fallback for an older installed soldr.
    assert "predates the mode" in text


def test_repo_guidance_names_the_bounded_mode() -> None:
    text = (ROOT / "CLAUDE.md").read_text(encoding="utf-8")
    assert PORCELAIN_ENV in text
    assert "log_path" in text
