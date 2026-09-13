"""Process coverage: all local rm requests use guaranteed dry-run, never a shell."""

from __future__ import annotations

import json
import os
import shutil
import sys
from pathlib import Path

import pytest

from tests import process


def binary(name: str) -> Path:
    suffix = ".exe" if sys.platform == "win32" else ""
    clud = os.environ.get("CLUD_TEST_BINARY")
    candidate = (
        Path(clud).with_name(name + suffix)
        if clud
        else Path(__file__).resolve().parents[1] / "target/debug" / (name + suffix)
    )
    assert candidate.is_file(), f"build {name} before running process tests"
    return candidate


@pytest.fixture
def shim(tmp_path: Path) -> Path:
    target = tmp_path / ("rm.exe" if sys.platform == "win32" else "rm")
    shutil.copy2(binary("clud-shim"), target)
    return target


@pytest.mark.parametrize(
    "args",
    [
        ["-rf", "/"],
        ["-rf", ""],
        ["-rf", "../escape"],
        ["--no-preserve-root", "/"],
        ["-rf", "safe", "/"],
    ],
)
def test_rm_alias_denies_in_dry_run_without_daemon(shim: Path, args: list[str]) -> None:
    env = os.environ.copy()
    env.pop("CLUD_DAEMON_SOCKET", None)
    env["CLUD_RM_DRY_RUN"] = "1"
    result = process.run([str(shim), *args], env=env, capture_output=True, text=True, timeout=30)
    assert result.returncode == 2, result
    assert json.loads(result.stdout)["decision"] == "deny"


@pytest.mark.skipif(
    sys.platform != "linux", reason="mount validation is Linux-only; other platforms deny"
)
def test_rm_alias_safe_dry_run_preserves_file(shim: Path, tmp_path: Path) -> None:
    target = tmp_path / "disposable"
    target.write_text("must survive")
    env = os.environ.copy() | {"CLUD_RM_DRY_RUN": "1", "TEST_CI": ""}
    result = process.run(
        [str(shim), "-rf", "--", str(target)], env=env, capture_output=True, text=True, timeout=30
    )
    assert result.returncode == 0, result
    assert json.loads(result.stdout)["dry_run"] is True
    assert target.read_text() == "must survive"


@pytest.mark.parametrize("state", ["trusted", "replaced", "missing", "system", "unreadable"])
def test_hook_rm_identity(shim: Path, tmp_path: Path, state: str) -> None:
    directory = tmp_path / state
    directory.mkdir()
    target = directory / shim.name
    if state == "trusted":
        shutil.copy2(shim, target)
    elif state == "replaced":
        target.write_text("replaced")
        target.chmod(0o755)
    elif state == "system":
        system = Path("/bin/rm")
        if not system.exists():
            pytest.skip("no Unix system rm")
        shutil.copyfile(system, target)
        target.chmod(0o755)
    elif state == "unreadable":
        if sys.platform == "win32":
            pytest.skip("POSIX executable permissions")
        shutil.copy2(shim, target)
        target.chmod(0)
    env = os.environ.copy() | {"PATH": str(directory)}
    payload = json.dumps(
        {"tool_name": "Bash", "cwd": str(tmp_path), "tool_input": {"command": "rm -rf ./build"}}
    )
    result = process.run(
        [str(binary("clud-block-bad-cmd"))],
        env=env,
        input=payload,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == (0 if state == "trusted" else 2), result
    if state != "trusted":
        assert json.loads(result.stdout)["hookSpecificOutput"]["permissionDecision"] == "deny"
        assert "rm identity" in result.stdout


@pytest.mark.parametrize(
    "command",
    [
        "/bin/rm ./build",
        "PATH=/bin; rm ./build",
        "command -p rm ./build",
        "busybox rm ./build",
        "hash -p /bin/rm rm",
        "bash -c 'rm ./build'",
    ],
)
def test_hook_refuses_resolution_bypasses(shim: Path, tmp_path: Path, command: str) -> None:
    payload = json.dumps(
        {"tool_name": "Bash", "cwd": str(tmp_path), "tool_input": {"command": command}}
    )
    env = os.environ.copy() | {"PATH": str(shim.parent)}
    result = process.run(
        [str(binary("clud-cmd-scan"))],
        env=env,
        input=payload,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 2, result
    assert "rm identity" in result.stdout
