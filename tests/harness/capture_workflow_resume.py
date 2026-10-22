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
    if event.get("tool_name") != "Workflow":
        return 0
    epochs_path = Path(sys.argv[1])
    epochs = json.loads(epochs_path.read_text())
    if isinstance(epochs, list):
        if not epochs:
            return 0
        next_epochs = epochs[0]
    else:
        if original.get("resumeFromRunId"):
            return 0
        next_epochs = epochs
    response = event.get("tool_response")
    if (not isinstance(response, dict)
            or not response.get("runId") or not response.get("scriptPath")):
        return 0
    args = {**original["args"], "gateEpochs": next_epochs}
    payload = {
        "scriptPath": response["scriptPath"], "resumeFromRunId": response["runId"], "args": args,
    }
    target = Path(sys.argv[2])
    temporary = target.with_suffix(".tmp")
    temporary.write_text(json.dumps(payload))
    temporary.replace(target)
    if isinstance(epochs, list):
        epochs_path.write_text(json.dumps(epochs[1:]))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
