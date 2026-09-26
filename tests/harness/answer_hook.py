"""PreToolUse answerer for AskUserQuestion in the real-harness tests (#1402).

`claude -p` has no user to answer `AskUserQuestion`. This hook fills the
answers in: argv[1] names a JSON file mapping a question's text (or its
header) to the option label to pick, with ``"*"`` as the default. A question
with no match takes its first option. The call is allowed with
``updatedInput.answers`` set; every other tool passes through silently.
"""

from __future__ import annotations

import json
import sys
from typing import Any


def _label(question: dict[str, Any], table: dict[str, str]) -> str:
    for key in (question.get("question"), question.get("header"), "*"):
        if isinstance(key, str) and key in table:
            return table[key]
    options = question.get("options") or []
    if options and isinstance(options[0], dict):
        return str(options[0].get("label", ""))
    return ""


def main() -> int:
    try:
        event = json.load(sys.stdin)
    except ValueError:
        return 0
    if event.get("tool_name") != "AskUserQuestion":
        return 0
    with open(sys.argv[1], encoding="utf-8") as fh:
        table = json.load(fh)
    tool_input = event.get("tool_input") or {}
    answers = {
        str(q.get("question", "")): _label(q, table)
        for q in tool_input.get("questions") or []
        if isinstance(q, dict)
    }
    output = {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": {**tool_input, "answers": answers},
        }
    }
    print(json.dumps(output))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
