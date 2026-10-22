"""A scheduled OpenRouter session keeps executing after refusing plan mode."""

from __future__ import annotations

from tests.harness.harness import Harness, tool_result_text, tool_results

MODEL = "nvidia/nemotron-3-ultra-550b-a55b:free"


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
    result = harness.run_clud(
        "Schedule the maintenance task and implement it",
        script,
        model=MODEL,
        extra_env={"CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "0"},
        record_scheduler=True,
    )
    assert result.returncode == 0, result.stdout[-3000:] + result.stderr[-3000:]
    failed = [request for request in result.requests if "MOCK_EXPECT_FAILED" in str(request)]
    scheduling = [hook for hook in result.hooks if hook.get("event") == "PostToolUse"]
    assert not failed, str(scheduling)[-4000:]
    assert any(
        block.get("is_error") and "loop-plan-mode" in tool_result_text(block)
        for block in tool_results(result)
    ), result.stdout[-4000:]
    assert (harness.repo / "loop-continued.txt").read_text() == "continued"
    assert "CONTINUED_AFTER_PLAN_DENIAL" in result.stdout
