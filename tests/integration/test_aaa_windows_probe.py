"""Minimum-surface integration probes that run *before* any other file in
`tests/integration/` (pytest collects alphabetically, and `aaa` beats
`daemon`).

Every test here uses `subprocess.run(..., timeout=...)` so if clud hangs the
test fails after N seconds with a TimeoutExpired — pytest-timeout is a second
line of defense.

Scope: run a single clud invocation, with bounded I/O, and verify it exits.
These tests intentionally *avoid* PTYs and the detach/daemon flow so they
discriminate "clud binary works on this platform at all" from "the daemon
fork hangs on Windows". Diagnostic only — if they pass on CI and
`test_daemon_centralized.py` hangs, the bug is in the daemon path.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

pytestmark = pytest.mark.integration


def test_probe_version_completes_quickly(clud_binary: Path) -> None:
    """`clud --version` is a pure stdout write — any Windows hang here means
    the binary itself is wedged before main() finishes, not a daemon bug."""
    result = subprocess.run(
        [str(clud_binary), "--version"],
        capture_output=True,
        text=True,
        timeout=15,
    )
    assert result.returncode == 0, (
        f"clud --version failed: rc={result.returncode} "
        f"stdout={result.stdout!r} stderr={result.stderr!r}"
    )
    assert "clud" in result.stdout.lower()


def test_probe_help_completes_quickly(clud_binary: Path) -> None:
    """clap help rendering — exercises argument parsing only."""
    result = subprocess.run(
        [str(clud_binary), "--help"],
        capture_output=True,
        text=True,
        timeout=15,
    )
    assert result.returncode == 0, (
        f"clud --help failed: rc={result.returncode} stderr={result.stderr!r}"
    )


def test_probe_list_with_no_daemon_state(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    """`clud list` with an empty state dir shouldn't even contact a daemon."""
    env = mock_env.copy()
    env["CLUD_DAEMON_STATE_DIR"] = str(tmp_path / "empty-state")
    result = subprocess.run(
        [str(clud_binary), "list"],
        capture_output=True,
        text=True,
        timeout=15,
        env=env,
    )
    assert result.returncode == 0, (
        f"clud list failed: rc={result.returncode} stderr={result.stderr!r}"
    )


def test_probe_logs_with_no_sessions(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    """`clud logs` over an empty state dir should print a friendly message."""
    env = mock_env.copy()
    env["CLUD_DAEMON_STATE_DIR"] = str(tmp_path / "empty-state")
    result = subprocess.run(
        [str(clud_binary), "logs"],
        capture_output=True,
        text=True,
        timeout=15,
        env=env,
    )
    assert result.returncode == 0, (
        f"clud logs failed: rc={result.returncode} stderr={result.stderr!r}"
    )


def test_probe_dry_run_emits_json(clud_binary: Path) -> None:
    """`clud --dry-run -p hello` writes the command plan to stdout as JSON
    without spawning anything. A hang here points at argv parsing, env probe,
    or the plan builder."""
    result = subprocess.run(
        [str(clud_binary), "--dry-run", "-p", "probe"],
        capture_output=True,
        text=True,
        timeout=15,
    )
    assert result.returncode == 0, (
        f"clud --dry-run failed: rc={result.returncode} stderr={result.stderr!r}"
    )
    assert "command" in result.stdout, f"no plan in stdout: {result.stdout!r}"
