"""Read actual native completion results, excluding echoed workflow source."""
from __future__ import annotations

import html
import json
import re
from typing import Any

from tests.harness.harness import RunResult
from tests.harness.test_workflow_gate_evidence import _message_texts


def workflow_notices(result: RunResult) -> dict[str, str]:
    notices = {}
    for row in result.requests:
        if row.get("role") != "main":
            continue
        for text in _message_texts(row.get("messages", [])):
            for match in re.finditer(r"<task-notification>(.*?)</task-notification>",
                                     text, re.DOTALL):
                notice = match[1]
                identity = re.search(r"<task-id>([^<]+)</task-id>", notice)
                if identity:
                    notices[identity[1]] = notice
    return notices


def workflow_results(result: RunResult) -> list[Any]:
    notices = workflow_notices(result)
    assert notices, "no native Workflow completion notification"
    outputs = []
    for notice in notices.values():
        assert "<status>completed</status>" in notice, notice[-4000:]
        payload = re.search(r"<result>(.*?)</result>", notice, re.DOTALL)
        assert payload, notice[-4000:]
        outputs.append(json.loads(html.unescape(payload[1])))
    return outputs
