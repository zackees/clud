"""PreToolUse recorder for the real-harness tests (#1323).

Appends one JSON line per tool call to the file named on the command line:
the tool, the calling agent's type and id, and the tool input. It always
allows the call; clud's real `clud-cmd-scan` is registered after it and makes
the actual decision.
"""

from __future__ import annotations

import json
import sys


def main() -> int:
    try:
        event = json.load(sys.stdin)
    except ValueError:
        return 0
    record = {
        "event": event.get("hook_event_name"),
        "tool_name": event.get("tool_name"),
        "agent_type": event.get("agent_type"),
        "agent_id": event.get("agent_id"),
        "session_id": event.get("session_id"),
        "tool_input": event.get("tool_input"),
        "prompt": event.get("prompt"),
    }
    with open(sys.argv[1], "a", encoding="utf-8") as log:
        log.write(json.dumps(record) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
