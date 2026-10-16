"""End to end: the real `clud --openrouter -p` refuses worktrees the user never
allowed (#1984).

clud launches the real Claude Code against a scripted fake OpenRouter model
(`mock-agent serve`, through the debug-only `CLUD_TEST_DIRECT_UPSTREAM_URL`
seam). The fake model creates a worktree on its own, whatever the prompt
says. clud's own hooks decide: `UserPromptSubmit` records consent in clud's
daemon only when the prompt says yes, and PreToolUse refuses otherwise. The
JSONL stream shows the refusal as an error `tool_result`, and the model's
next request (the script's `expect`) sees the same.
"""

from __future__ import annotations

from typing import Any

from tests.harness.harness import Harness, RunResult, tool_result_text, tool_results

DENIED = "Git worktrees are off for this session"


def _worktree_script(harness: Harness, *, expect_error: bool, done: str) -> dict[str, Any]:
    destination = harness.home / ".clud" / "worktrees" / "repo-wt-e2e"
    return {
        "default_text": done,
        "roles": [
            {
                "name": "main",
                "steps": [
                    {
                        "tool_use": {
                            "name": "Bash",
                            "input": {
                                "command": f"git worktree add -b fix/e2e {destination}",
                                "description": "isolate the fix",
                            },
                        }
                    },
                    {
                        "expect": {"is_error": expect_error}
                        | ({"content_contains": DENIED} if expect_error else {}),
                        "text": done,
                    },
                ],
            }
        ],
    }


def _assert_script_held(result: RunResult) -> None:
    failed = [r for r in result.requests if "MOCK_EXPECT_FAILED" in str(r.get("step"))]
    assert not failed, failed


def _denials(result: RunResult) -> list[str]:
    return [
        tool_result_text(block)
        for block in tool_results(result)
        if block.get("is_error") and DENIED in tool_result_text(block)
    ]


def test_no_mention_of_worktrees_and_the_models_worktree_is_denied(harness: Harness) -> None:
    script = _worktree_script(harness, expect_error=True, done="SAW_THE_DENIAL")

    result = harness.run_clud("I need you to fix this bug", script)

    assert "SAW_THE_DENIAL" in result.stdout, result.stdout[-3000:] + result.stderr[-3000:]
    _assert_script_held(result)
    assert _denials(result), "the JSONL stream must carry the refusal"
    assert "has not allowed" in _denials(result)[0]
    assert not (harness.home / ".clud" / "worktrees" / "repo-wt-e2e").exists()
    # The prompt went through clud's UserPromptSubmit hook and recorded nothing.
    assert any(h.get("event") == "UserPromptSubmit" for h in result.hooks)


def test_a_prompt_that_says_no_still_denies(harness: Harness) -> None:
    script = _worktree_script(harness, expect_error=True, done="SAW_THE_DENIAL")

    result = harness.run_clud("Fix this bug. No worktrees, just do it here.", script)

    assert "SAW_THE_DENIAL" in result.stdout, result.stdout[-3000:] + result.stderr[-3000:]
    _assert_script_held(result)
    assert _denials(result)


def test_a_prompt_that_allows_worktrees_lets_the_model_make_one(harness: Harness) -> None:
    script = _worktree_script(harness, expect_error=False, done="WORKTREE_MADE")

    result = harness.run_clud("You can use worktrees. I need you to fix this bug.", script)

    assert "WORKTREE_MADE" in result.stdout, result.stdout[-3000:] + result.stderr[-3000:]
    _assert_script_held(result)
    assert not _denials(result)
    assert (harness.home / ".clud" / "worktrees" / "repo-wt-e2e").is_dir()
