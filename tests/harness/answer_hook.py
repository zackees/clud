"""PreToolUse answerer for AskUserQuestion in the real-harness tests (#1402).

`claude -p` has no user to answer `AskUserQuestion`. This hook fills the
answers in: argv[1] names a JSON file mapping a question's text (or its
header) to the option label to pick, with ``"*"`` as the default. A list of
labels answers a multi-select question (joined with ``", "``, as Claude Code
does). A question with no match takes its first option. The call is allowed
with ``updatedInput.answers`` set, which Claude Code accepts as the user's
answer ("Hook satisfied user interaction ... via updatedInput"); every other
tool passes through silently.

With an optional argv[2], every answered question is appended to that JSONL
file with the time it was asked (``t``, Unix milliseconds, the clock the
mock backend's ``t_start``/``t_end`` use) and the asking agent's type.

In print mode Claude Code only *offers* AskUserQuestion when a permission
prompt tool is configured, so the harness also passes
``--permission-prompt-tool`` naming ``answer_mcp.py``, which answers from the
same table if the hook path is ever bypassed. See ``Harness.run(answers=)``.
"""

from __future__ import annotations

import json
import sys
import time
from typing import Any


def _label(question: dict[str, Any], table: dict[str, Any]) -> str:
    for key in (question.get("question"), question.get("header"), "*"):
        if isinstance(key, str) and key in table:
            value = table[key]
            if isinstance(value, list):
                return ", ".join(str(v) for v in value)
            return str(value)
    options = question.get("options") or []
    if options and isinstance(options[0], dict):
        return str(options[0].get("label", ""))
    return ""


def answer(
    tool_input: dict[str, Any],
    table: dict[str, Any],
    log_path: str | None = None,
    agent_type: str | None = None,
) -> dict[str, Any]:
    """`tool_input` with ``answers`` filled from `table`; logs each question."""
    questions = [q for q in tool_input.get("questions") or [] if isinstance(q, dict)]
    answers = {str(q.get("question", "")): _label(q, table) for q in questions}
    if log_path:
        now = int(time.time() * 1000)
        with open(log_path, "a", encoding="utf-8") as log:
            for q in questions:
                record = {
                    "t": now,
                    "agent_type": agent_type,
                    "question": q.get("question"),
                    "header": q.get("header"),
                    "answer": answers[str(q.get("question", ""))],
                }
                log.write(json.dumps(record) + "\n")
    return {**tool_input, "answers": answers}


def main() -> int:
    try:
        event = json.load(sys.stdin)
    except ValueError:
        return 0
    if event.get("tool_name") != "AskUserQuestion":
        return 0
    with open(sys.argv[1], encoding="utf-8") as fh:
        table = json.load(fh)
    log_path = sys.argv[2] if len(sys.argv) > 2 else None
    updated = answer(event.get("tool_input") or {}, table, log_path, event.get("agent_type"))
    output = {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": updated,
        }
    }
    print(json.dumps(output))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
