"""Copy real native Workflow references into a later mocked-model tool call.

This only copies the first launch's actual references and arguments. It never
changes tool results, journal entries, authorization decisions or gate evidence.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path


def main() -> int:
    event = json.load(sys.stdin)
    original = event.get("tool_input", {})
    if event.get("tool_name") != "Workflow" or original.get("resumeFromRunId"):
        return 0
    response = event.get("tool_response")
    if not isinstance(response, dict) or not response.get("runId") or not response.get("scriptPath"):
        return 0
    epochs = json.loads(Path(sys.argv[1]).read_text())
    args = {**original["args"], "gateEpochs": epochs}
    payload = {"scriptPath": response["scriptPath"], "resumeFromRunId": response["runId"], "args": args}
    target = Path(sys.argv[2])
    temporary = target.with_suffix(".tmp")
    temporary.write_text(json.dumps(payload))
    temporary.replace(target)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
