"""Fixtures for integration tests with mock agents."""

from __future__ import annotations

import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent.parent


def _find_clud() -> Path:
    """Build the current repo's clud binary and return its path."""
    result = subprocess.run(
        ["cargo", "build", "-p", "clud", "--message-format=json"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(f"Failed to build clud:\n{result.stderr}")

    import json

    for line in result.stdout.splitlines():
        msg = json.loads(line)
        if (
            msg.get("reason") == "compiler-artifact"
            and msg.get("target", {}).get("name") == "clud"
            and msg.get("executable")
        ):
            return Path(msg["executable"])

    ext = ".exe" if sys.platform == "win32" else ""
    fallback = ROOT / "target" / "debug" / f"clud{ext}"
    if fallback.is_file():
        return fallback
    raise RuntimeError("clud binary not found after build")


def _build_mock_agent() -> Path:
    """Build the mock-agent binary and return its path."""
    result = subprocess.run(
        ["cargo", "build", "-p", "mock-agent", "--message-format=json"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(f"Failed to build mock-agent:\n{result.stderr}")

    import json

    for line in result.stdout.splitlines():
        msg = json.loads(line)
        if (
            msg.get("reason") == "compiler-artifact"
            and msg.get("target", {}).get("name") == "mock-agent"
            and msg.get("executable")
        ):
            return Path(msg["executable"])

    # Fallback: look in target/debug
    ext = ".exe" if sys.platform == "win32" else ""
    fallback = ROOT / "target" / "debug" / f"mock-agent{ext}"
    if fallback.is_file():
        return fallback
    raise RuntimeError("mock-agent binary not found after build")


@pytest.fixture(scope="session")
def mock_agent_binary() -> Path:
    """Build mock-agent once per test session."""
    return _build_mock_agent()


@pytest.fixture
def mock_env(mock_agent_binary: Path, tmp_path: Path) -> dict[str, str]:
    """Create a temp directory with mock claude/codex binaries on PATH.

    Returns an environment dict with PATH set so that `claude` and `codex`
    resolve to the mock-agent binary.
    """
    ext = ".exe" if platform.system() == "Windows" else ""

    # Copy mock-agent as both claude and codex
    claude_path = tmp_path / f"claude{ext}"
    codex_path = tmp_path / f"codex{ext}"
    shutil.copy2(mock_agent_binary, claude_path)
    shutil.copy2(mock_agent_binary, codex_path)

    if platform.system() != "Windows":
        claude_path.chmod(0o755)
        codex_path.chmod(0o755)

    # Build env with mock dir first on PATH
    env = os.environ.copy()
    env["PATH"] = str(tmp_path) + os.pathsep + env.get("PATH", "")
    # Prevent any VIRTUAL_ENV interference
    env.pop("VIRTUAL_ENV", None)

    # Enable PID logging for zombie detection
    pid_log = tmp_path / "child_pids.log"
    env["RUNNING_PROCESS_CHILD_PID_LOG_PATH"] = str(pid_log)

    return env


@pytest.fixture
def clud_binary() -> Path:
    """Return the path to the current repo's clud binary."""
    return _find_clud()


def _scan_for_clud_zombies() -> list[dict]:
    """Scan the system for orphaned CLUD-spawned processes."""
    scan_bin = ROOT / "target" / "debug" / "examples" / "scan_zombies.exe"
    if not scan_bin.is_file():
        scan_bin = ROOT / "target" / "debug" / "examples" / "scan_zombies"
    if not scan_bin.is_file():
        return []
    try:
        result = subprocess.run(
            [str(scan_bin)],
            capture_output=True,
            text=True,
            timeout=10,
        )
        orphans = []
        for line in result.stdout.splitlines():
            if "ORPHAN" in line:
                orphans.append({"line": line.strip()})
        return orphans
    except Exception:
        return []


@pytest.fixture(autouse=True)
def _check_no_zombies_after_test():
    """After each test, verify no orphaned CLUD processes were leaked."""
    yield
    import time

    time.sleep(0.2)  # brief settle time for process cleanup
    orphans = _scan_for_clud_zombies()
    if orphans:
        msg = "CLUD zombie processes detected after test:\n"
        for o in orphans:
            msg += f"  {o['line']}\n"
        pytest.fail(msg)
