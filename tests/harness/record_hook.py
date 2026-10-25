"""PreToolUse recorder for the real-harness tests (#1323).

Appends one JSON line per tool call to the file named on the command line:
the tool, the calling agent's type and id, and the tool input. It always
allows the call; clud's real `clud-cmd-scan` is registered after it and makes
the actual decision.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path


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
        "payload_keys": sorted(event),
        "transcript_path": event.get("transcript_path"),
        "agent_transcript_path": event.get("agent_transcript_path"),
        "cwd": event.get("cwd"),
        "tool_input": event.get("tool_input"),
        "prompt": event.get("prompt"),
    }
    if event.get("tool_name") == "Workflow":
        transcript = Path(event.get("transcript_path") or "/missing-transcript")
        try:
            with transcript.open("rb") as source:
                source.seek(max(0, transcript.stat().st_size - 256 * 1024))
                tail = source.read()
            rows = []
            for line in tail.splitlines():
                try:
                    row = json.loads(line)
                except ValueError:
                    continue
                if "version" in row:
                    rows.append({key: row.get(key) for key in ("type", "sessionId", "version")})
            record["transcript_versions"] = rows[-8:]
        except OSError as error:
            record["transcript_read_error"] = str(error)
    if "tool_response" in event:
        record["tool_response"] = event["tool_response"]
    with open(sys.argv[1], "a", encoding="utf-8") as log:
        log.write(json.dumps(record) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
