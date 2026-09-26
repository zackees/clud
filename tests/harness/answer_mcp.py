"""Stub MCP permission-prompt server for the real-harness tests (#1402).

Claude Code in print mode (``claude -p``) leaves ``AskUserQuestion`` out of
the tools it offers the model unless a permission prompt tool is configured
(``--permission-prompt-tool``): with nobody to ask, the tool is disabled.
``Harness.run(answers=...)`` therefore starts this stdio MCP server with
``--mcp-config`` and names its one tool, ``mcp__clud_harness__answer``, as
the permission prompt tool. That makes the tool available; the PreToolUse
hook ``answer_hook.py`` then answers each call through ``updatedInput``.

If Claude Code ever routes an AskUserQuestion call here instead, this server
answers it from the same table (argv[1]) and logs it (argv[2]). Every other
permission request is denied, which is what print mode does without a
prompt tool, so enabling the question tool changes nothing else.

Protocol: newline-delimited JSON-RPC 2.0 on stdin/stdout (MCP stdio).
"""

from __future__ import annotations

import json
import sys
from typing import Any

try:
    from tests.harness.answer_hook import answer
except ImportError:  # run as a script: tests/harness is sys.path[0]
    from answer_hook import answer  # type: ignore[no-redef]

SERVER = "clud_harness"
TOOL = "answer"
TOOL_NAME = f"mcp__{SERVER}__{TOOL}"

_SCHEMA = {
    "type": "object",
    "properties": {
        "tool_name": {"type": "string"},
        "input": {"type": "object"},
        "tool_use_id": {"type": "string"},
    },
    "required": ["tool_name", "input"],
}


def decide(arguments: dict[str, Any], table: dict[str, Any], log_path: str | None) -> dict:
    """The permission decision for one prompt-tool call."""
    tool_input = arguments.get("input") or {}
    if arguments.get("tool_name") == "AskUserQuestion":
        return {"behavior": "allow", "updatedInput": answer(tool_input, table, log_path)}
    return {
        "behavior": "deny",
        "message": "clud harness: no interactive approval in print mode",
    }


def handle(message: dict[str, Any], table: dict[str, Any], log_path: str | None) -> dict | None:
    """The JSON-RPC response to `message`, or None for a notification."""
    if "id" not in message:
        return None
    method = message.get("method")
    params = message.get("params") or {}
    if method == "initialize":
        result: dict[str, Any] = {
            "protocolVersion": params.get("protocolVersion", "2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SERVER, "version": "1"},
        }
    elif method == "ping":
        result = {}
    elif method == "tools/list":
        result = {
            "tools": [
                {
                    "name": TOOL,
                    "description": "Test harness permission prompt: answers AskUserQuestion.",
                    "inputSchema": _SCHEMA,
                }
            ]
        }
    elif method == "tools/call" and params.get("name") == TOOL:
        decision = decide(params.get("arguments") or {}, table, log_path)
        result = {"content": [{"type": "text", "text": json.dumps(decision)}]}
    else:
        return {
            "jsonrpc": "2.0",
            "id": message["id"],
            "error": {"code": -32601, "message": f"method not found: {method}"},
        }
    return {"jsonrpc": "2.0", "id": message["id"], "result": result}


def main() -> int:
    with open(sys.argv[1], encoding="utf-8") as fh:
        table = json.load(fh)
    log_path = sys.argv[2] if len(sys.argv) > 2 else None
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if not isinstance(message, dict):
            continue
        response = handle(message, table, log_path)
        if response is not None:
            sys.stdout.write(json.dumps(response) + "\n")
            sys.stdout.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
