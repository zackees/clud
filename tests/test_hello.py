"""Verify core clud CLI behavior: --help, --version, --dry-run, pipe mode."""

from __future__ import annotations

import json
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Don't use tomllib: it's py311-only and `requires-python = ">=3.10"` promises
# py310 works. A regex is sufficient for the trivial `version = "x.y.z"` line
# in [project] and avoids the tomli backport dep.
_VERSION_RE = re.compile(r'^version\s*=\s*"([^"]+)"', re.MULTILINE)


def _project_version() -> str:
    text = (ROOT / "pyproject.toml").read_text(encoding="utf-8")
    match = _VERSION_RE.search(text)
    if match is None:
        raise RuntimeError("project.version missing from pyproject.toml")
    return match.group(1)


def _cargo_argv(subcommand: list[str]) -> list[str]:
    """Return the cargo argv, preferring `soldr cargo` on Windows (issue #27).

    Windows rustc installations from chocolatey ship a GNU-host rustc which
    links C/C++ deps against MinGW runtime DLLs (libstdc++-6.dll,
    libgcc_s_seh-1.dll, libwinpthread-1.dll). Those DLLs aren't on stock
    Windows, so a test that launches the resulting binary fails with
    STATUS_ENTRYPOINT_NOT_FOUND (0xC0000139). `soldr cargo ...` forces the
    MSVC target via the rustup-managed toolchain. Mirrors
    `tests/integration/conftest.py::_cargo_argv`.
    """
    if sys.platform == "win32":
        soldr = shutil.which("soldr")
        if soldr:
            return [soldr, "cargo", *subcommand]
    return ["cargo", *subcommand]


def _clud_binary() -> str:
    """Build the current repo's clud binary and return its path."""
    result = subprocess.run(
        _cargo_argv(["build", "-p", "clud", "--message-format=json"]),
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=120,
    )
    if result.returncode != 0:
        raise RuntimeError(f"Failed to build clud:\n{result.stderr}")

    for line in result.stdout.splitlines():
        msg = json.loads(line)
        if (
            msg.get("reason") == "compiler-artifact"
            and msg.get("target", {}).get("name") == "clud"
            and msg.get("executable")
        ):
            return msg["executable"]

    ext = ".exe" if sys.platform == "win32" else ""
    # soldr / `--target x86_64-pc-windows-msvc` lands artifacts in a
    # triple-qualified subdir; bare cargo lands in target/debug. Check both.
    for fallback in (
        ROOT / "target" / "x86_64-pc-windows-msvc" / "debug" / f"clud{ext}",
        ROOT / "target" / "aarch64-pc-windows-msvc" / "debug" / f"clud{ext}",
        ROOT / "target" / "debug" / f"clud{ext}",
    ):
        if fallback.is_file():
            return str(fallback)
    raise RuntimeError("clud binary not found after build")


CLUD = _clud_binary()
PROJECT_VERSION = _project_version()


def _run(*args: str, input_data: str | None = None) -> subprocess.CompletedProcess[str]:
    with tempfile.TemporaryDirectory() as temp_dir:
        source = Path(CLUD)
        launch = Path(temp_dir) / source.name
        shutil.copy2(source, launch)
        return subprocess.run(
            [str(launch), *args],
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
    assert result.stdout.strip() == f"clud {PROJECT_VERSION}"


def test_dry_run_prompt() -> None:
    result = _run("--dry-run", "-p", "hello")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert data["backend"] == "claude"
    assert data["launch_mode"] == "subprocess"
    assert "--dangerously-skip-permissions" in data["command"]
    assert "-p" in data["command"]
    assert "hello" in data["command"]
    assert data["iterations"] == 1


def test_dry_run_codex() -> None:
    result = _run("--dry-run", "--codex", "-p", "hello")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert data["backend"] == "codex"
    assert data["launch_mode"] == "subprocess"


def test_dry_run_pty_override() -> None:
    result = _run("--dry-run", "--pty", "-p", "hello")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert data["backend"] == "claude"
    assert data["launch_mode"] == "pty"


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
    result = _run("--dry-run", "up", "--publish")
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
    # Prompt is the last arg (Claude's `-p <prompt>`), with the DONE-marker
    # contract appended. Original task is preserved at the start.
    prompt = data["command"][-1]
    assert prompt.startswith("do stuff")
    assert ".clud/loop/DONE" in prompt
    assert ".clud/loop/BLOCKED" in prompt
    assert data["loop_markers"] is not None
    assert data["loop_markers"]["done_path"].replace("\\", "/").endswith(".clud/loop/DONE")
    assert data["loop_markers"]["blocked_path"].replace("\\", "/").endswith(
        ".clud/loop/BLOCKED"
    )


def test_dry_run_loop_no_done() -> None:
    result = _run("--dry-run", "loop", "--no-done", "do stuff")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert data["command"][-1] == "do stuff"
    assert data["loop_markers"] is None


def test_dry_run_loop_repeat_implies_no_done() -> None:
    result = _run("--dry-run", "loop", "--repeat", "1h", "do stuff")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    assert data["command"][-1] == "do stuff"
    assert data["loop_markers"] is None
    assert data["repeat_interval_secs"] == 3600


def test_dry_run_loop_repeat_with_done_override() -> None:
    result = _run("--dry-run", "loop", "--repeat", "1h", "--done", "DONE.md", "do stuff")
    assert result.returncode == 0
    data = json.loads(result.stdout)
    prompt = data["command"][-1]
    assert "DONE.md" in prompt
    assert "BLOCKED.md" in prompt
    assert data["loop_markers"]["done_path"].replace("\\", "/").endswith("DONE.md")
    assert data["loop_markers"]["blocked_path"].replace("\\", "/").endswith("BLOCKED.md")


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
