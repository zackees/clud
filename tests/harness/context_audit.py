"""Assertions over actual mock-agent HTTP captures; byte counts are not tokens."""

from __future__ import annotations

import json
from typing import Any


def audit_context(
    requests: list[dict[str, Any]],
    *,
    instruction_marker: str,
    output_markers: list[str],
    initial_budget: int,
    growth_budget: int,
) -> dict[str, Any]:
    """Reject duplicates and unaccounted growth, including unchanged-history replay."""
    assert len(requests) == len(output_markers) + 1, "unexpected model request amplification"
    first = requests[0]["context_bytes"]
    assert first["request"] <= initial_budget, "initial context exceeds its reviewed budget"
    previous_bytes = first["request"]
    previous_results: dict[str, Any] = {}
    sizes = []
    for turn, request in enumerate(requests):
        measured = request["context_bytes"]
        assert measured["request"] >= sum(measured[key] for key in ("system", "tools", "messages"))
        assert measured["tools_fingerprint"] == first["tools_fingerprint"], "tool schemas changed"
        assert measured["system"] == first["system"], "system context grew"
        visible = json.dumps([request.get("system"), request.get("messages")])
        assert visible.count(instruction_marker) == 1, "instructions were duplicated or lost"
        _check_repeated_text(request)
        results = _tool_results(request)
        ids = [block["tool_use_id"] for block in results]
        assert len(ids) == len(set(ids)), "tool-result history was duplicated"
        assert len(results) == turn, "tool-result history grew unexpectedly"
        for block in results:
            identity = block["tool_use_id"]
            if identity in previous_results:
                assert block.get("content") == previous_results[identity], "historical output changed"
            previous_results[identity] = block.get("content")
        for index, marker in enumerate(output_markers):
            count = sum(json.dumps(block.get("content")).count(marker) for block in results)
            assert count == int(index < turn), "historical tool output was duplicated or lost"
        if turn:
            assert measured["request"] - previous_bytes <= growth_budget, "unexplained context growth"
        previous_bytes = measured["request"]
        sizes.append({key: measured[key] for key in ("request", "system", "tools", "messages")})
    return {"request_count": len(requests), "cumulative_request_bytes": sum(x["request"] for x in sizes),
            "initial_request_bytes": sizes[0]["request"], "requests": sizes}


def _tool_results(request: dict[str, Any]) -> list[dict[str, Any]]:
    return [block for message in request.get("messages", [])
            if isinstance(message.get("content"), list)
            for block in message["content"]
            if isinstance(block, dict) and block.get("type") == "tool_result"]


def _check_repeated_text(request: dict[str, Any]) -> None:
    texts = []
    for message in request.get("messages", []):
        content = message.get("content")
        if isinstance(content, str):
            texts.append(content)
        elif isinstance(content, list):
            texts.extend(block["text"] for block in content
                         if isinstance(block, dict) and isinstance(block.get("text"), str))
    long_blocks = [text for text in texts if len(text.encode()) >= 512]
    assert len(long_blocks) == len(set(long_blocks)), "large context block was repeated"
