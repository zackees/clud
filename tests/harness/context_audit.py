"""Assertions over actual mock-agent HTTP captures; byte counts are not tokens."""

from __future__ import annotations

import json
import re
from typing import Any


def audit_context(
    requests: list[dict[str, Any]],
    *,
    instruction_marker: str,
    output_markers: list[str],
    initial_budget: int,
    growth_budget: int,
    forbidden_markers: tuple[str, ...] = (),
    history_counts: list[int] | None = None,
    expected_payloads: list[str] | None = None,
    extra_context_texts: dict[int, list[tuple[str, str]]] | None = None,
    expected_tool_calls: list[dict[str, Any]] | None = None,
    extra_assistant_texts: dict[int, list[str]] | None = None,
    instruction_sections: list[str] | None = None,
) -> dict[str, Any]:
    """Reject duplicates and unaccounted growth, including unchanged-history replay."""
    expected = (
        history_counts if history_counts is not None else list(range(len(output_markers) + 1))
    )
    assert len(requests) == len(expected), "unexpected model request amplification"
    first = requests[0]["context_bytes"]
    assert first["request"] <= initial_budget, "initial context exceeds its reviewed budget"
    previous_bytes = first["request"]
    previous_results: dict[str, Any] = {}
    previous_ids: list[str] = []
    previous_history: list[tuple[str, Any]] = []
    baseline_context = _message_context(requests[0])
    sizes = []
    for turn, request in enumerate(requests):
        history_count = expected[turn]
        measured = request["context_bytes"]
        assert measured["request"] >= sum(measured[key] for key in ("system", "tools", "messages"))
        assert measured["tools_fingerprint"] == first["tools_fingerprint"], "tool schemas changed"
        assert measured["system"] == first["system"], "system context grew"
        assert measured["system_fingerprint"] == first["system_fingerprint"], (
            "system context changed"
        )
        expected_context = baseline_context + (extra_context_texts or {}).get(turn, [])
        _check_message_shapes(request)
        previous_history = _check_history_prefix(request, previous_history)
        assert _message_context(request) == expected_context, (
            "unexpected, repeated or changed context text"
        )
        visible = json.dumps([request.get("system"), request.get("messages")])
        assert visible.count(instruction_marker) == 1, "instructions were duplicated or lost"
        assert not any(marker in visible for marker in forbidden_markers), (
            "unrelated context leaked"
        )
        assert not request["unexpected_context"], "unrelated context leaked outside messages"
        _check_repeated_text(request)
        _check_instruction_sections(request, instruction_sections or [])
        if expected_tool_calls is not None:
            _check_assistant_history(
                request,
                expected_tool_calls[:history_count],
                (extra_assistant_texts or {}).get(turn, []),
            )
        results = _tool_results(request)
        ids = [block["tool_use_id"] for block in results]
        assert len(ids) == len(set(ids)), "tool-result history was duplicated"
        assert ids[: len(previous_ids)] == previous_ids, "historical call/result IDs changed"
        previous_ids = ids
        assert len(results) == history_count, "tool-result history grew unexpectedly"
        for block in results:
            identity = block["tool_use_id"]
            if identity in previous_results:
                assert block.get("content") == previous_results[identity], (
                    "historical output changed"
                )
            previous_results[identity] = block.get("content")
        _check_outputs(results, output_markers, history_count, expected_payloads)
        if turn:
            assert measured["request"] - previous_bytes <= growth_budget, (
                "unexplained context growth"
            )
        previous_bytes = measured["request"]
        sizes.append({key: measured[key] for key in ("request", "system", "tools", "messages")})
    return {
        "request_count": len(requests),
        "cumulative_request_bytes": sum(x["request"] for x in sizes),
        "initial_request_bytes": sizes[0]["request"],
        "requests": sizes,
    }


def _tool_results(request: dict[str, Any]) -> list[dict[str, Any]]:
    return [
        block
        for message in request.get("messages", [])
        if isinstance(message.get("content"), list)
        for block in message["content"]
        if isinstance(block, dict) and block.get("type") == "tool_result"
    ]


def _check_history_prefix(
    request: dict[str, Any], previous: list[tuple[str, Any]]
) -> list[tuple[str, Any]]:
    history = []
    for message in request["messages"]:
        content = message["content"]
        blocks = [{"type": "text", "text": content}] if isinstance(content, str) else content
        for block in blocks:
            history.append(
                (
                    message["role"],
                    {key: value for key, value in block.items() if key != "cache_control"},
                )
            )
    assert history[: len(previous)] == previous, (
        "complete historical context was reordered or changed"
    )
    return history


def _texts(content: Any) -> list[str]:
    if isinstance(content, str):
        return [content] if content.strip() else []
    if not isinstance(content, list):
        return []
    return [
        text
        for block in content
        for text in _texts(block.get("text") if isinstance(block, dict) else block)
    ]


def _message_context(request: dict[str, Any]) -> list[tuple[str, str]]:
    return [
        (message["role"], text)
        for message in request.get("messages", [])
        if message.get("role") in {"user", "system"}
        for text in _texts(message.get("content"))
    ]


def _check_repeated_text(request: dict[str, Any]) -> None:
    texts = _texts(request.get("system_blocks", request.get("system")))
    texts.extend(text for _, text in _message_context(request))
    assert len(texts) == len(set(texts)), "context block was repeated"


def _check_outputs(
    results: list[dict[str, Any]],
    markers: list[str],
    count: int,
    payloads: list[str] | None,
) -> None:
    assert all(block.get("is_error", False) is False for block in results), (
        "scripted successful tool result changed to an error"
    )
    if payloads is not None:
        assert [block.get("content") for block in results] == payloads[:count], (
            "unexpected or corrupted tool payload"
        )
    texts = [
        block["content"]
        if isinstance(block.get("content"), str)
        else json.dumps(block.get("content"), ensure_ascii=False)
        for block in results
    ]
    for index, marker in enumerate(markers):
        assert sum(text.count(marker) for text in texts) == int(index < count), (
            "historical tool output was duplicated or lost"
        )


def audit_retry(first: dict[str, Any], retry: dict[str, Any]) -> None:
    assert (
        first["context_bytes"]["body_fingerprint"] == retry["context_bytes"]["body_fingerprint"]
    ), "retry changed the received request body"
    assert first["messages"] == retry["messages"], "retry changed conversation history"
    assert first["context_bytes"] == retry["context_bytes"], "retry changed context metrics"


def _check_instruction_sections(request: dict[str, Any], sections: list[str]) -> None:
    texts = _texts(request.get("system_blocks", request.get("system")))
    texts.extend(text for _, text in _message_context(request))
    for section in sections:
        assert sum(text.count(section) for text in texts) == 1, (
            "instruction section repeated or lost"
        )
    paragraphs = [
        paragraph.strip()
        for text in _texts(request.get("system_blocks"))
        for paragraph in re.split(r"\n\s*\n", text)
        if len(paragraph.strip()) >= 32
    ]
    assert len(paragraphs) == len(set(paragraphs)), "system instruction paragraph was repeated"


def _check_assistant_history(
    request: dict[str, Any],
    expected: list[dict[str, Any]],
    expected_texts: list[str],
) -> None:
    messages = [message for message in request["messages"] if message.get("role") == "assistant"]
    _check_assistant_blocks(messages)
    texts = [text for message in messages for text in _texts(message.get("content"))]
    assert texts == expected_texts, "unexpected or repeated assistant context"
    calls = [
        block
        for message in messages
        if isinstance(message.get("content"), list)
        for block in message["content"]
        if isinstance(block, dict) and block.get("type") == "tool_use"
    ]
    assert len(calls) == len(expected), "unexpected assistant tool calls"
    assert [{"name": call["name"], "input": call["input"]} for call in calls] == expected, (
        "assistant tool inputs changed or contain unrelated context"
    )
    assert [call["id"] for call in calls] == [
        result["tool_use_id"] for result in _tool_results(request)
    ], "tool call and result identities disagree"
    _check_tool_sequence(request, calls)


def _check_tool_sequence(request: dict[str, Any], calls: list[dict[str, Any]]) -> None:
    sequence = [
        (block["type"], block.get("id", block.get("tool_use_id")))
        for message in request["messages"]
        if isinstance(message.get("content"), list)
        for block in message["content"]
        if block.get("type") in {"tool_use", "tool_result"}
    ]
    assert sequence == [
        pair for call in calls for pair in (("tool_use", call["id"]), ("tool_result", call["id"]))
    ], "scripted tool call/result chronology changed"


def _check_message_shapes(request: dict[str, Any]) -> None:
    fields = {
        "text": {"type", "text", "cache_control"},
        "tool_use": {"type", "id", "name", "input", "cache_control"},
        "tool_result": {"type", "tool_use_id", "content", "is_error", "cache_control"},
    }
    for message in request["messages"]:
        assert set(message) <= {"role", "content"}, "unexpected message fields"
        role = message.get("role")
        assert role in {"user", "assistant", "system"}, "unexpected message role"
        content = message.get("content")
        assert isinstance(content, str | list), "unexpected message content"
        for block in content if isinstance(content, list) else []:
            assert isinstance(block, dict), "unexpected block shape"
            assert block.get("type") in fields, "unexpected block type"
            kind = block["type"]
            assert set(block) <= fields[kind], "unexpected context fields"
            _check_cache_control(block)
            assert kind != "tool_use" or role == "assistant", "unexpected tool call role"
            assert kind != "tool_result" or role == "user", "unexpected tool result role"


def _check_cache_control(block: dict[str, Any]) -> None:
    if "cache_control" in block:
        assert block["cache_control"] in [
            {"type": "ephemeral"},
            {"type": "ephemeral", "ttl": "5m"},
            {"type": "ephemeral", "ttl": "1h"},
        ], "unexpected cache-control context"


def _check_assistant_blocks(messages: list[dict[str, Any]]) -> None:
    fields = {
        "text": {"type", "text", "cache_control"},
        "tool_use": {"type", "id", "name", "input", "cache_control"},
    }
    for message in messages:
        content = message.get("content")
        assert isinstance(content, list), "unexpected assistant content shape"
        for block in content:
            assert isinstance(block, dict), "unexpected assistant block shape"
            assert block.get("type") in fields, "unexpected assistant block type"
            assert set(block) <= fields[block["type"]], "unexpected assistant block fields"
