"""Regression tests for bundled hook stdin handling."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BLOCK_BAD_CMD = (
    ROOT / "crates" / "clud-bin" / "assets" / "tools" / "hooks" / "block-bad-cmd.py"
)


def _hook_env(home: Path) -> dict[str, str]:
    env = os.environ.copy()
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
    env["CLUD_HOOK_STDIN_IDLE_TIMEOUT_SEC"] = "0.05"
    env["CLUD_HOOK_STDIN_DEADLINE_SEC"] = "0.25"
    return env


def _run_hook_with_open_stdin(
    tmp_path: Path,
    payload: str | None,
) -> subprocess.CompletedProcess[str]:
    proc = subprocess.Popen(
        [sys.executable, str(BLOCK_BAD_CMD)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=_hook_env(tmp_path / "home"),
    )
    assert proc.stdin is not None
    assert proc.stdout is not None
    assert proc.stderr is not None
    if payload is not None:
        proc.stdin.write(payload)
        proc.stdin.flush()

    try:
        returncode = proc.wait(timeout=2.0)
    except subprocess.TimeoutExpired:
        proc.kill()
        stdout, stderr = proc.communicate(timeout=1.0)
        raise AssertionError(
            f"hook did not exit while stdin pipe remained open; stdout={stdout!r} "
            f"stderr={stderr!r}"
        ) from None

    proc.stdin.close()
    stdout = proc.stdout.read()
    stderr = proc.stderr.read()
    return subprocess.CompletedProcess(proc.args, returncode, stdout, stderr)


def test_block_bad_cmd_reads_payload_without_waiting_for_stdin_eof(tmp_path: Path) -> None:
    payload = json.dumps(
        {
            "tool_name": "Bash",
            "tool_input": {"command": "bad" + " cmd"},
        }
    )

    result = _run_hook_with_open_stdin(tmp_path, payload)

    assert result.returncode == 2
    assert "permissionDecision" in result.stdout
    assert "deny" in result.stdout
    assert "refusing to run" in result.stderr


def test_block_bad_cmd_allows_missing_payload_without_waiting_for_stdin_eof(
    tmp_path: Path,
) -> None:
    result = _run_hook_with_open_stdin(tmp_path, None)

    assert result.returncode == 0
    log_path = tmp_path / "home" / ".clud" / "tools" / "hooks" / "block-bad-cmd.log"
    log = log_path.read_text(encoding="utf-8")
    assert "stdin_read_incomplete" in log
    assert "raw_stdin_bytes=0" in log
