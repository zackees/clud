#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# managed-by: clud
"""telemetry.py — PostToolUse hook that ships one record to the clud daemon.

Hook contract (mirrors Claude Code PostToolUse payloads):
- Input: JSON object on stdin with `tool_name`, `tool_input`,
  `tool_response`, and (optionally) `cwd`, `session_id`.
- Output: nothing actionable. This hook NEVER blocks a tool call —
  it ALWAYS exits 0 regardless of what happens internally.

Invoked via `clud tool run hooks/telemetry.py` so UV_CACHE_DIR is pinned
to ~/.clud/cache/uv per the three-layer enforcement (issue #408). clud
auto-installs this file to ~/.clud/tools/hooks/telemetry.py on every
startup; the `# managed-by: clud` marker above is what gates the
installer's overwrite behavior. Hand-edit at your own risk — drop the
marker if you want the installer to leave your copy alone.

Behavior:
- If `$CLUD_DAEMON_HTTP_SERVER` is unset/empty, exits 0 silently.
- Otherwise POSTs a JSON envelope to `<server>/telemetry/log` matching
  the daemon's `TelemetryIngest` schema (parent_pid, time_ms, cmd, cwd,
  env where every key starts with `CLUD_`).
- 2s HTTP timeout — hook callers must not hang on a dead daemon.
- Stdlib only (urllib.request); no third-party deps in the venv.

Recommended `~/.claude/settings.json` wiring (matcher "*", async):

    "PostToolUse": [{
      "matcher": "*",
      "hooks": [{
        "type": "command",
        "command": "clud tool run hooks/telemetry.py",
        "async": true,
        "timeout": 30
      }]
    }]
"""

from __future__ import annotations

import json
import os
import sys
import time
import urllib.request
from typing import Any

# Tight cap — hook callers must never wait on a stuck daemon.
HTTP_TIMEOUT_SEC = 2.0
# Truncate the cmd summary so the dashboard table stays readable and
# the in-memory ring buffer doesn't bloat on huge tool_input blobs.
CMD_MAX_LEN = 200


def _cmd_summary(payload: dict[str, Any]) -> str:
    """Pick the most useful one-line summary for the `cmd` field.

    The daemon's `TelemetryIngest` schema has a single `cmd: String`
    field; we lossy-summarize tool-specific input down to something a
    human reading the dashboard's per-PID table can scan at a glance.
    """
    tool_name = payload.get("tool_name", "?")
    tool_input = payload.get("tool_input") or {}

    if tool_name == "Bash":
        return f"Bash: {tool_input.get('command', '')}"
    if tool_name in ("Edit", "Write", "Read", "NotebookEdit"):
        return f"{tool_name}: {tool_input.get('file_path', '')}"
    if tool_name in ("Grep", "Glob"):
        return f"{tool_name}: {tool_input.get('pattern', '')}"
    # Fallback: tool_name plus a truncated JSON snapshot of input.
    try:
        snippet = json.dumps(tool_input, ensure_ascii=False, default=str)
    except Exception:
        snippet = repr(tool_input)
    if len(snippet) > CMD_MAX_LEN - len(tool_name) - 2:
        snippet = snippet[: CMD_MAX_LEN - len(tool_name) - 5] + "..."
    return f"{tool_name}: {snippet}"


def main() -> int:
    try:
        server = os.environ.get("CLUD_DAEMON_HTTP_SERVER", "").strip()
        if not server:
            return 0  # No daemon configured. Silent no-op.

        raw = sys.stdin.read()
        if not raw.strip():
            return 0

        try:
            payload = json.loads(raw)
        except Exception:
            return 0  # Malformed hook payload — nothing actionable.

        body = {
            "parent_pid": os.getppid(),
            "time_ms": int(time.time() * 1000),
            "cmd": _cmd_summary(payload),
            # Hook payload's `cwd` reflects Claude Code's cwd at fire
            # time; fall back to ours when absent.
            "cwd": payload.get("cwd") or os.getcwd(),
            # Every CLUD_* env var, verbatim. Callers can use this as a
            # tagging mechanism (e.g. CLUD_SESSION_ID, CLUD_TASK, ...).
            "env": {k: v for k, v in os.environ.items() if k.startswith("CLUD_")},
        }

        url = server.rstrip("/") + "/telemetry/log"
        req = urllib.request.Request(
            url,
            data=json.dumps(body).encode("utf-8"),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(req, timeout=HTTP_TIMEOUT_SEC):
            pass
    except Exception:
        # Swallow EVERYTHING. The only contract is "exit 0".
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
