"""Observe native workflow metadata at PreToolUse time; capability probe only."""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path


def observe(event: dict, config: Path) -> dict:
    agent = event.get("agent_id")
    session = event.get("session_id")
    row = {
        "agent_id": agent, "session_id": session, "agent_type": event.get("agent_type"),
        "transcript_path": event.get("transcript_path"), "cwd": event.get("cwd"),
        "command": event.get("tool_input", {}).get("command"), "metadata": [],
    }
    if not all(isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9_-]+", value)
               for value in (agent, session)):
        return row
    pattern = f"*/{session}/subagents/workflows/*/agent-{agent}.meta.json"
    for path in sorted((config / "projects").glob(pattern)):
        metadata = json.loads(path.read_text(encoding="utf-8"))
        row["metadata"].append({
            "path": str(path.relative_to(config)),
            "agent_type": metadata.get("agentType"),
            "label": metadata.get("description"),
        })
    return row


def main() -> None:
    event = json.load(sys.stdin)
    row = observe(event, Path(sys.argv[1]))
    with Path(sys.argv[2]).open("a", encoding="utf-8") as stream:
        stream.write(json.dumps(row) + "\n")


if __name__ == "__main__":
    main()
