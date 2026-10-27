"""Mutate real captures, so a passing audit must also reject realistic faults."""

from __future__ import annotations

from copy import deepcopy
from typing import Any


def _result_message(request: dict[str, Any]) -> dict[str, Any]:
    return next(
        message
        for message in request["messages"]
        if isinstance(message.get("content"), list)
        and any(
            block.get("type") == "tool_result"
            for block in message["content"]
            if isinstance(block, dict)
        )
    )


def _result(request: dict[str, Any]) -> dict[str, Any]:
    return next(
        block
        for block in _result_message(request)["content"]
        if isinstance(block, dict) and block.get("type") == "tool_result"
    )


def adversarial_cases(requests: list[dict[str, Any]], instruction: str, canary: str):
    rows = deepcopy(requests)
    rows[1]["system"] += instruction
    yield "duplicate instructions", rows

    rows = deepcopy(requests)
    _result_message(rows[1])["content"].append(deepcopy(_result(rows[1])))
    yield "duplicate result ID", rows

    rows = deepcopy(requests)
    extra = deepcopy(_result(rows[1]))
    extra["tool_use_id"] += "-duplicate"
    _result_message(rows[1])["content"].append(extra)
    yield "duplicate result with new ID", rows

    rows = deepcopy(requests)
    _result(rows[-1])["content"] = "historical output was overwritten"
    yield "altered old output", rows

    rows = deepcopy(requests)
    message = _result_message(rows[-1])
    message["content"].remove(_result(rows[-1]))
    yield "lost old output", rows

    rows = deepcopy(requests)
    rows[1]["messages"].extend([{"role": "user", "content": "repeated notice " * 100}] * 2)
    yield "repeated long reminder", rows

    rows = deepcopy(requests)
    rows[1]["messages"].append({"role": "user", "content": "unexpected metadata " * 3000})
    rows[1]["context_bytes"]["request"] += 60_000
    rows[1]["context_bytes"]["messages"] += 60_000
    yield "unexpected context growth", rows

    rows = deepcopy(requests)
    rows[1]["context_bytes"]["tools_fingerprint"] = "mutated"
    yield "schema churn", rows

    rows = deepcopy(requests)
    rows.append(deepcopy(rows[-1]))
    yield "unexpected model call", rows

    rows = deepcopy(requests)
    rows[1]["messages"].append({"role": "user", "content": canary})
    yield "unrelated context leakage", rows

    rows = deepcopy(requests)
    for row in rows[2:]:
        results = [
            block
            for message in row["messages"]
            if isinstance(message.get("content"), list)
            for block in message["content"]
            if isinstance(block, dict) and block.get("type") == "tool_result"
        ]
        marker = results[1]["content"].partition(": ")[0]
        results[1]["content"] = marker + ": " + results[0]["content"].partition(": ")[2]
    yield "copied payload with preserved ID and marker", rows

    rows = deepcopy(requests)
    for row in rows[2:]:
        results = [
            block
            for message in row["messages"]
            if isinstance(message.get("content"), list)
            for block in message["content"]
            if isinstance(block, dict) and block.get("type") == "tool_result"
        ]
        results[1]["content"] += " Unnecessary repeated guidance."
    yield "unrequested text appended to tool output", rows

    rows = deepcopy(requests)
    rows[1]["messages"].extend([{"role": "user", "content": "Short repeated reminder."}] * 2)
    yield "repeated short reminders below growth budget", rows

    rows = deepcopy(requests)
    rows[1]["messages"].extend(
        [
            {"role": "user", "content": "New wrapper. Shared reminder sentence."},
            {"role": "user", "content": "Shared reminder sentence. Another wrapper."},
        ]
    )
    yield "repeated sentence inside different wrappers", rows

    rows = deepcopy(requests)
    blocks = rows[0]["system_blocks"]
    rows[0]["system_blocks"] = (
        [*blocks, deepcopy(blocks[0])] if isinstance(blocks, list) else [blocks, blocks]
    )
    yield "duplicate unmarked initial system block", rows

    rows = deepcopy(requests)
    rows[1]["context_bytes"]["system_fingerprint"] = "same-size-system-mutation"
    yield "same-size system context mutation", rows

    rows = deepcopy(requests)
    assistant = next(
        message for message in rows[1]["messages"] if message.get("role") == "assistant"
    )
    assistant["content"].extend([{"type": "text", "text": "Repeated assistant note."}] * 2)
    yield "short repeated assistant text", rows

    rows = deepcopy(requests)
    for row in rows[1:]:
        assistant = next(
            message for message in row["messages"] if message.get("role") == "assistant"
        )
        call = next(block for block in assistant["content"] if block.get("type") == "tool_use")
        call["input"]["unrelated_context"] = "not part of the scripted command"
    yield "unrelated content in assistant tool inputs", rows

    rows = deepcopy(requests)
    block = next(block for block in rows[0]["system_blocks"] if len(block.get("text", "")) > 100)
    block["text"] += "\n\n" + block["text"]
    yield "duplicated section inside initial system block", rows

    rows = deepcopy(requests)
    paragraph = "Unmarked instruction paragraph with enough meaningful text to expose repetition."
    rows[0]["system_blocks"].extend(
        [
            {"type": "text", "text": "First wrapper.\n\n" + paragraph},
            {"type": "text", "text": paragraph + "\n\nDifferent wrapper."},
        ]
    )
    yield "repeated system section in different wrappers", rows

    rows = deepcopy(requests)
    rows[1]["unexpected_context"] = True
    yield "canary outside flattened messages or system text", rows

    rows = deepcopy(requests)
    assistant = next(
        message for message in rows[1]["messages"] if message.get("role") == "assistant"
    )
    assistant["content"].extend(
        [{"type": "thinking", "thinking": "Unnecessary hidden repetition."}] * 2
    )
    yield "unexpected repeated thinking blocks", rows

    rows = deepcopy(requests)
    for row in rows[2:]:
        assistant = next(
            message for message in row["messages"] if message.get("role") == "assistant"
        )
        call = next(block for block in assistant["content"] if block.get("type") == "tool_use")
        original = call["id"]
        call["id"] += "-rewritten"
        for message in row["messages"]:
            for block in message["content"] if isinstance(message.get("content"), list) else []:
                if isinstance(block, dict) and block.get("tool_use_id") == original:
                    block["tool_use_id"] = call["id"]
    yield "paired historical call/result IDs rewritten together", rows

    rows = deepcopy(requests)
    rows[1]["messages"].append(
        {
            "role": "user",
            "content": [
                {"type": "image", "source": {"type": "base64", "data": "unrequested context"}}
            ],
        }
    )
    yield "unexpected image content", rows

    rows = deepcopy(requests)
    _result(rows[1])["extra_context"] = "unrequested repeated context"
    yield "unexpected tool-result metadata", rows

    rows = deepcopy(requests)
    rows[1]["messages"][0]["extra_context"] = "unrequested repeated context"
    yield "unexpected message metadata", rows

    rows = deepcopy(requests)
    messages = rows[1]["messages"]
    assistant = next(
        index for index, message in enumerate(messages) if message["role"] == "assistant"
    )
    result = messages.index(_result_message(rows[1]))
    messages[assistant], messages[result] = messages[result], messages[assistant]
    yield "result precedes its tool call", rows

    rows = deepcopy(requests)
    _result(rows[-1])["is_error"] = True
    yield "historical successful result changed to error", rows

    rows = deepcopy(requests)
    _result(rows[1])["cache_control"] = {"type": "ephemeral", "text": "unrequested context"}
    yield "unexpected content in cache metadata", rows

    rows = deepcopy(requests)
    messages = rows[1]["messages"]
    original = next(index for index, message in enumerate(messages) if message["role"] == "user")
    prompt = messages.pop(original)
    assistant = next(
        index for index, message in enumerate(messages) if message["role"] == "assistant"
    )
    messages.insert(assistant + 1, prompt)
    yield "original prompt moved inside a tool turn", rows
