"""AskUserQuestion answered by the harness on the real Claude Code (#1402).

The model calls `AskUserQuestion` with one question (options ``red`` and
``blue``); ``Harness.run(answers=...)`` registers ``answer_hook.py``, which
allows the call with ``updatedInput.answers`` filled in. The test checks that
the chosen answer reaches the model in the next request's tool result.

If `claude -p` ever stops honouring ``updatedInput.answers`` for
AskUserQuestion, the fallback is a ``--permission-prompt-tool`` stub that
returns the answers instead (#1402).
"""

from __future__ import annotations

import json

from tests.harness.harness import Harness, questions_before

QUESTION = {
    "question": "Which colour?",
    "header": "Colour",
    "multiSelect": False,
    "options": [
        {"label": "red", "description": "the red one"},
        {"label": "blue", "description": "the blue one"},
    ],
}

SCRIPT = {
    "default_text": "ACK",
    "roles": [
        {
            "name": "main",
            "steps": [
                {"tool_use": {"name": "AskUserQuestion", "input": {"questions": [QUESTION]}}},
                {"text": "ACK"},
            ],
        }
    ],
}


def _tool_results(request: dict) -> str:
    found = []
    for message in request.get("messages") or []:
        content = message.get("content")
        if isinstance(content, list):
            found += [b for b in content if isinstance(b, dict) and b.get("type") == "tool_result"]
    return json.dumps(found)


def test_ask_user_question_is_answered(harness: Harness) -> None:
    result = harness.run("pick a colour", SCRIPT, answers={"*": "blue"})
    assert result.returncode == 0, result.stdout[-2000:]
    mains = [r for r in result.requests if r.get("role") == "main"]
    assert len(mains) >= 2, [r.get("role") for r in result.requests]
    followup = _tool_results(mains[1])
    assert "blue" in followup, followup[:2000]
    assert len(questions_before(result, "grind-planner")) == 1
