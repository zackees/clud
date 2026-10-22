"""Loop-scoped plan-mode policy through the real hook and isolated daemon."""

from __future__ import annotations

import json
import os
import shutil
from collections.abc import Iterator
from pathlib import Path

import pytest

from tests import process

SESSION = "loop-plan-mode-1844"
DENIED = "loop-plan-mode"


@pytest.fixture
def loop_world(tmp_path: Path) -> Iterator[tuple[str, dict[str, str], Path]]:
    binary = os.environ.get("CLUD_TEST_BINARY") or shutil.which("clud")
    assert binary, "build the clud test binary first"
    home = tmp_path / "home"
    home.mkdir()
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / ".git").mkdir()
    env = os.environ.copy()
    for name in tuple(env):
        if name.startswith("CLUD_"):
            env.pop(name)
    env.update(
        {
            "HOME": str(home),
            "USERPROFILE": str(home),
            "CLUD_HOOK_HOME": str(home),
            "CLUD_DAEMON_STATE_DIR": str(tmp_path / "state"),
            "CLUD_DAEMON_TEST_MODE": "1",
            "CLUD_INTEGRATION_TESTS": "1",
            "CLUD_NO_UNLOCK": "1",
            "CLUD_SERVER_SETTINGS": "0",
            "SOLDR_BROKER_AUTOSPAWN": "0",
        }
    )
    try:
        started = process.run(
            [binary, "daemon", "api-info", "--json"],
            env=env,
            capture_output=True,
            text=True,
            timeout=60,
        )
        assert started.returncode == 0, started.stdout + started.stderr
        yield binary, env, repo
    finally:
        process.run(
            [binary, "daemon", "stop"],
            env=env,
            capture_output=True,
            text=True,
            timeout=30,
        )


def _hook(
    world: tuple[str, dict[str, str], Path], event: str, **fields: object
) -> process.CompletedProcess[str]:
    binary, env, repo = world
    return process.run(
        [binary, "__cmd-scan", "--event", event],
        input=json.dumps(
            {"session_id": SESSION, "cwd": str(repo), "hook_event_name": event, **fields}
        ),
        env=env,
        cwd=repo,
        capture_output=True,
        text=True,
        timeout=30,
    )


def test_loop_prompt_denies_plan_mode(loop_world: tuple[str, dict[str, str], Path]) -> None:
    prompt = _hook(loop_world, "UserPromptSubmit", prompt="/loop 20m fix the next issue")
    assert prompt.returncode == 0, prompt.stderr
    result = _hook(loop_world, "PreToolUse", tool_name="EnterPlanMode", tool_input={})
    assert result.returncode == 2, result.stdout + result.stderr
    decision = json.loads(result.stdout)["hookSpecificOutput"]
    assert decision["permissionDecision"] == "deny"
    assert DENIED in decision["permissionDecisionReason"]
