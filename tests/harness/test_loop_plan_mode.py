"""A scheduled OpenRouter session keeps executing after refusing plan mode."""

from __future__ import annotations

import json

import pytest

from tests.harness.harness import Harness

MODEL = "nvidia/nemotron-3-ultra-550b-a55b:free"


@pytest.mark.timeout(180)
def test_scheduled_session_refuses_plan_mode_and_continues(harness: Harness) -> None:
    script = {
        "default_text": "CONTINUED_AFTER_PLAN_DENIAL",
        "roles": [
            {
                "name": "main",
                "steps": [
                    {
                        "tool_use": {
                            "name": "CronCreate",
                            "input": {
                                "cron": "*/20 * * * *",
                                "prompt": "fix the next issue",
                                "recurring": True,
                            },
                        }
                    },
                    {
                        "expect": {"is_error": False},
                        "tool_use": {"name": "EnterPlanMode", "input": {}},
                    },
                    {
                        "expect": {"is_error": True, "content_contains": "loop-plan-mode"},
                        "tool_use": {
                            "name": "Bash",
                            "input": {
                                "command": "printf 'continued' > loop-continued.txt",
                                "description": "continue the authorized task after the refusal",
                            },
                        },
                    },
                    {"expect": {"is_error": False}, "text": "CONTINUED_AFTER_PLAN_DENIAL"},
                ],
            }
        ],
    }
    result = harness.run_clud_interactive(
        "Schedule the maintenance task and implement it",
        script,
        model=MODEL,
        finished="CONTINUED_AFTER_PLAN_DENIAL",
    )
    assert result.returncode == 0, result.stdout[-3000:] + result.stderr[-3000:]
    failed = [request for request in result.requests if "MOCK_EXPECT_FAILED" in str(request)]
    scheduling = [hook for hook in result.hooks if hook.get("event") == "PostToolUse"]
    assert not failed, str(scheduling)[-4000:]
    assert any(hook.get("tool_name") == "EnterPlanMode" for hook in result.hooks)
    assert any("loop-plan-mode" in json.dumps(request) for request in result.requests)
    assert (harness.repo / "loop-continued.txt").read_text() == "continued"
    assert "CONTINUED_AFTER_PLAN_DENIAL" in result.stdout
