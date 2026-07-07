"""Regression tests for bundled hook stdin handling."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TELEMETRY = (
    ROOT / "crates" / "clud-bin" / "assets" / "tools" / "hooks" / "telemetry.py"
)
CLAUDE_SOLDR_HOOK = ROOT / ".claude" / "hooks" / "check-soldr.py"
CODEX_SOLDR_HOOK = ROOT / ".codex" / "hooks" / "check-soldr.py"


def _hook_env(home: Path) -> dict[str, str]:
    env = os.environ.copy()
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
    env["CLUD_HOOK_STDIN_IDLE_TIMEOUT_SEC"] = "0.05"
    env["CLUD_HOOK_STDIN_DEADLINE_SEC"] = "0.25"
    env["CLUD_TELEMETRY_STDIN_IDLE_TIMEOUT_SEC"] = "0.05"
    env["CLUD_TELEMETRY_STDIN_DEADLINE_SEC"] = "0.25"
    return env


def _binary_name(name: str) -> str:
    return f"{name}.exe" if sys.platform == "win32" else name


def _block_bad_cmd_binary() -> Path:
    env_binary = os.environ.get("CLUD_TEST_BLOCK_BAD_CMD_BINARY")
    if env_binary and Path(env_binary).is_file():
        return Path(env_binary)

    clud_binary = os.environ.get("CLUD_TEST_BINARY")
    if clud_binary:
        sibling = Path(clud_binary).with_name(_binary_name("clud-block-bad-cmd"))
        if sibling.is_file():
            return sibling

    resolved = shutil.which(_binary_name("clud-block-bad-cmd"))
    if resolved:
        return Path(resolved)

    raise AssertionError("clud-block-bad-cmd test binary not found")


def _run_hook_with_open_stdin(
    tmp_path: Path,
    payload: str | None,
    argv: list[str] | None = None,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    env = _hook_env(tmp_path / "home")
    if extra_env:
        env.update(extra_env)
    if argv is None:
        argv = [str(_block_bad_cmd_binary())]
    proc = subprocess.Popen(
        argv,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
    )
    assert proc.stdin is not None
    assert proc.stdout is not None
    assert proc.stderr is not None
    if payload is not None:
        proc.stdin.write(payload)
        proc.stdin.flush()

    try:
        returncode = proc.wait(timeout=5.0)
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


def test_block_bad_cmd_allows_malformed_json(tmp_path: Path) -> None:
    result = _run_hook_with_open_stdin(tmp_path, "{not-json")

    assert result.returncode == 0
    assert "permissionDecision" not in result.stdout


def test_telemetry_hook_reads_payload_without_waiting_for_stdin_eof(
    tmp_path: Path,
) -> None:
    payload = json.dumps(
        {
            "tool_name": "Bash",
            "tool_input": {"command": "echo hi"},
        }
    )

    result = _run_hook_with_open_stdin(
        tmp_path,
        payload,
        argv=[sys.executable, str(TELEMETRY)],
        extra_env={"CLUD_DAEMON_HTTP_SERVER": "not-a-valid-url"},
    )

    assert result.returncode == 0


def test_tracked_soldr_hooks_read_payload_without_waiting_for_stdin_eof(
    tmp_path: Path,
) -> None:
    payload = json.dumps(
        {
            "tool_name": "Bash",
            "tool_input": {"command": "echo hi"},
        }
    )

    for script in (CLAUDE_SOLDR_HOOK, CODEX_SOLDR_HOOK):
        result = _run_hook_with_open_stdin(
            tmp_path,
            payload,
            argv=[sys.executable, str(script)],
        )
        assert result.returncode == 0, script
