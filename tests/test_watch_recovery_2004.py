"""Executable wait recovery through the real hook, without remote reads (#2004)."""

from __future__ import annotations

import json
import os
from pathlib import Path

import pytest

from tests import process

TOOLS = [("Bash", "command"), ("exec_command", "cmd"),
         ("functions.exec_command", "cmd"), ("shell_command", "command")]
WAITER = ("clud tool run github/pr_merge_watch.py -- 123 --repo zackees/clud "
          "--timeout 540 --require '.*' --no-cancel")


def scan(tmp_path: Path, tool: str, key: str, command: str) -> process.CompletedProcess[str]:
    binary = os.environ["CLUD_TEST_BINARY"]
    settings = tmp_path / ".clud" / "settings.json"
    settings.parent.mkdir(exist_ok=True)
    settings.write_text(json.dumps({"git": {"pr_wait_fail_fast": True}}), encoding="utf-8")
    env = {name: value for name, value in os.environ.items() if not name.startswith("CLUD_")}
    env.update(HOME=str(tmp_path), USERPROFILE=str(tmp_path), CLUD_HOOK_HOME=str(tmp_path),
               CLUD_SKIP_RM_IDENTITY="1", CLUD_NO_UNLOCK="1", CLUD_SERVER_SETTINGS="0")
    return process.run(
        [binary, "__cmd-scan", "--event", "PreToolUse"],
        input=json.dumps({"hook_event_name": "PreToolUse", "tool_name": tool,
                          "tool_input": {key: command}, "cwd": str(tmp_path)}),
        env=env, cwd=tmp_path, capture_output=True, text=True, timeout=20,
    )


@pytest.mark.parametrize(("tool", "key"), TOOLS)
def test_rejected_watch_offers_verified_execution_capabilities(
    tmp_path: Path, tool: str, key: str
) -> None:
    result = scan(tmp_path, tool, key, "gh run watch 123 --exit-status")
    assert result.returncode == 2, result.stdout + result.stderr
    reason = json.loads(result.stdout)["hookSpecificOutput"]["permissionDecisionReason"]
    for required in ["exec_command", "write_stdin", "empty input", "same session",
                     "run_in_background", "completion notification", "bounded foreground",
                     "capabilities", "tool name", "local process", "Never poll",
                     "pr_merge_watch.py -- <PR>"]:
        assert required in reason, f"missing {required!r}: {reason}"


@pytest.mark.parametrize(("tool", "key"), TOOLS)
def test_canonical_bounded_waiter_remains_allowed(tmp_path: Path, tool: str, key: str) -> None:
    result = scan(tmp_path, tool, key, WAITER)
    assert result.returncode == 0, result.stdout + result.stderr


@pytest.mark.parametrize(("tool", "key"), TOOLS)
def test_polling_loop_cannot_replace_session_waiting(tmp_path: Path, tool: str, key: str) -> None:
    result = scan(tmp_path, tool, key, "while gh pr checks 123; do sleep 5; done")
    assert result.returncode == 2, result.stdout + result.stderr
    assert "hand-written-wait" in result.stderr
