"""Verify core clud CLI behavior: --help, --version, --dry-run, pipe mode."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path


def _clud_binary() -> str:
    """Find the clud binary in the venv."""
    venv = Path(sys.executable).parent
    if sys.platform == "win32":
        candidate = venv / "clud.exe"
    else:
        candidate = venv / "clud"
    if candidate.is_file():
        return str(candidate)
    return "clud"


CLUD = _clud_binary()


def _run(*args: str, input_data: str | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [CLUD, *args],
        capture_output=True,
        text=True,
        timeout=10,
        input=input_data,
    )


def test_help() -> None:
    result = _run("--help")
    assert result.returncode == 0
    assert "YOLO" in result.stdout
    assert "--prompt" in result.stdout
    assert "--safe" in result.stdout
    assert "loop" in result.stdout


def test_version() -> None:
    result = _run("--version")
    assert result.returncode == 0
    assert "clud" in result.stdout
    assert "0.1.0" in result.stdout


def test_dry_run_prompt() -> None:
    result = _run("--dry-run", "-p", "hello")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert data["backend"] == "claude"
    assert "--dangerously-skip-permissions" in data["command"]
    assert "-p" in data["command"]
    assert "hello" in data["command"]
    assert data["iterations"] == 1


def test_dry_run_codex() -> None:
    result = _run("--dry-run", "--codex", "-p", "hello")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert data["backend"] == "codex"


def test_dry_run_safe_mode() -> None:
    result = _run("--dry-run", "--safe", "-p", "hello")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert "--dangerously-skip-permissions" not in data["command"]


def test_dry_run_model() -> None:
    result = _run("--dry-run", "--model", "opus", "-p", "hello")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert "--model" in data["command"]
    assert "opus" in data["command"]


def test_dry_run_continue() -> None:
    result = _run("--dry-run", "-c")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert "--continue" in data["command"]


def test_dry_run_message() -> None:
    result = _run("--dry-run", "-m", "fix bug")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert "-m" in data["command"]
    assert "fix bug" in data["command"]


def test_dry_run_up() -> None:
    result = _run("--dry-run", "up")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    prompt = data["command"][-1]
    assert "lint" in prompt.lower()
    assert "codeup" in prompt.lower()


def test_dry_run_up_with_message() -> None:
    result = _run("--dry-run", "up", "-m", "bump version")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    prompt = data["command"][-1]
    assert 'codeup -m "bump version"' in prompt


def test_dry_run_up_with_publish() -> None:
    result = _run("--dry-run", "up", "-p")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    prompt = data["command"][-1]
    assert "-p" in prompt.split("codeup")[1]


def test_dry_run_rebase() -> None:
    result = _run("--dry-run", "rebase")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    prompt = data["command"][-1]
    assert "git fetch" in prompt
    assert "rebase" in prompt.lower()


def test_dry_run_fix() -> None:
    result = _run("--dry-run", "fix")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    prompt = data["command"][-1].lower()
    assert "linting" in prompt
    assert "unit tests" in prompt


def test_dry_run_fix_with_url() -> None:
    result = _run("--dry-run", "fix", "https://github.com/user/repo/actions/runs/123")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    prompt = data["command"][-1]
    assert "github.com/user/repo/actions/runs/123" in prompt
    assert "gh run view" in prompt


def test_dry_run_loop() -> None:
    result = _run("--dry-run", "loop", "--loop-count", "5", "do stuff")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert data["iterations"] == 5
    assert "do stuff" in data["command"]


def test_dry_run_loop_default_count() -> None:
    result = _run("--dry-run", "loop", "task")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert data["iterations"] == 50


def test_dry_run_passthrough_flags() -> None:
    result = _run("--dry-run", "--unknown-flag", "-p", "hello")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert "--unknown-flag" in data["command"]


def test_pipe_mode() -> None:
    result = _run("--dry-run", input_data="piped prompt")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert "-p" in data["command"]
    assert "piped prompt" in data["command"]
