"""Negative controls for repeated-context and request-growth detection (#2225)."""

from __future__ import annotations

import copy

import pytest

from tests.harness.context_audit import audit_context, audit_retry


def fixture_requests() -> list[dict]:
    rows = []
    for turn in range(3):
        results = [
            {"type": "tool_result", "tool_use_id": f"tool-{i}", "content": f"OUTPUT_{i}"}
            for i in range(turn)
        ]
        rows.append(
            {
                "system": "INSTRUCTION_ONCE",
                "unexpected_context": False,
                "messages": [{"role": "user", "content": results}],
                "context_bytes": {
                    "request": 1000 + 100 * turn,
                    "system": 20,
                    "tools": 50,
                    "messages": 50 + 100 * turn,
                    "tools_fingerprint": "stable",
                    "system_fingerprint": "stable",
                },
            }
        )
    return rows


def check(rows: list[dict]) -> dict:
    return audit_context(
        rows,
        instruction_marker="INSTRUCTION_ONCE",
        output_markers=["OUTPUT_0", "OUTPUT_1"],
        initial_budget=1200,
        growth_budget=200,
    )


def test_openrouter_context_audit_accepts_append_only_history() -> None:
    report = check(fixture_requests())
    assert report["request_count"] == 3
    assert report["cumulative_request_bytes"] == 3300


@pytest.mark.parametrize(
    "mutation",
    ["instructions", "tool_result", "growth", "request", "schema", "long_text", "history"],
)
def test_openrouter_context_audit_rejects_context_bloat(mutation: str) -> None:
    rows = copy.deepcopy(fixture_requests())
    if mutation == "instructions":
        rows[1]["system"] += " INSTRUCTION_ONCE"
    elif mutation == "tool_result":
        rows[1]["messages"][0]["content"] *= 2
    elif mutation == "growth":
        rows[1]["context_bytes"]["request"] += 1000
    elif mutation == "request":
        rows.append(copy.deepcopy(rows[-1]))
    elif mutation == "schema":
        rows[1]["context_bytes"]["tools_fingerprint"] = "changed"
    elif mutation == "long_text":
        rows[1]["messages"].extend([{"role": "user", "content": "repeated notice " * 100}] * 2)
    else:
        rows[2]["messages"][0]["content"][0]["content"] += "changed"
    with pytest.raises(AssertionError):
        check(rows)


def test_openrouter_context_retry_detects_same_size_body_changes() -> None:
    first = fixture_requests()[0]
    first["context_bytes"]["body_fingerprint"] = "original"
    retry = copy.deepcopy(first)
    audit_retry(first, retry)
    retry["context_bytes"]["body_fingerprint"] = "same-size-different-body"
    with pytest.raises(AssertionError, match="request body"):
        audit_retry(first, retry)


def scripted_requests(*, escaped: bool = False) -> tuple[list[dict], list[str], list[dict]]:
    payloads = (
        ['OUTPUT_0: quotes " and backslash \\ and newline\n雪🙂', "OUTPUT_1: same line\nsame line"]
        if escaped
        else ["OUTPUT_0: first", "OUTPUT_1: second"]
    )
    calls = [{"name": "Bash", "input": {"command": f"emit {i}"}} for i in range(2)]
    rows = fixture_requests()
    for turn, row in enumerate(rows):
        row["messages"] = [
            {
                "role": "user",
                "content": [
                    {"type": "text", "text": "First instruction."},
                    {"type": "text", "text": "Second instruction."},
                ],
            }
        ]
        for index in range(turn):
            row["messages"].extend(
                [
                    {
                        "role": "assistant",
                        "content": [{"type": "tool_use", "id": f"tool-{index}", **calls[index]}],
                    },
                    {
                        "role": "user",
                        "content": [
                            {
                                "type": "tool_result",
                                "tool_use_id": f"tool-{index}",
                                "content": payloads[index],
                            }
                        ],
                    },
                ]
            )
    return rows, payloads, calls


def check_scripted(
    rows: list[dict],
    payloads: list[str],
    calls: list[dict],
    *,
    history_counts: list[int] | None = None,
) -> dict:
    return audit_context(
        rows,
        instruction_marker="INSTRUCTION_ONCE",
        output_markers=["OUTPUT_0", "OUTPUT_1"],
        initial_budget=1200,
        growth_budget=200,
        expected_payloads=payloads,
        expected_tool_calls=calls,
        history_counts=history_counts,
    )


def test_openrouter_context_preserves_escaped_unicode_and_legitimate_repeated_output() -> None:
    rows, payloads, calls = scripted_requests(escaped=True)
    assert check_scripted(rows, payloads, calls)["request_count"] == 3


@pytest.mark.parametrize(
    "mutation",
    [
        "unknown_block",
        "text_metadata",
        "result_metadata",
        "instruction_order",
        "output_pairing",
        "causal_order",
        "error_flag",
        "cache_metadata",
        "prompt_chronology",
    ],
)
def test_openrouter_context_rejects_hidden_content_and_reordered_history(mutation: str) -> None:
    rows, payloads, calls = scripted_requests()
    if mutation == "unknown_block":
        rows[1]["messages"][0]["content"].append(
            {"type": "image", "source": {"data": "unrequested context"}}
        )
    elif mutation == "text_metadata":
        rows[1]["messages"][0]["content"][0]["extra_context"] = "unrequested context"
    elif mutation == "result_metadata":
        rows[1]["messages"][-1]["content"][0]["extra_context"] = "unrequested context"
    elif mutation == "error_flag":
        rows[1]["messages"][-1]["content"][0]["is_error"] = True
    elif mutation == "cache_metadata":
        rows[1]["messages"][-1]["content"][0]["cache_control"] = {
            "type": "ephemeral",
            "text": "unrequested context",
        }
    elif mutation == "instruction_order":
        rows[1]["messages"][0]["content"].reverse()
    elif mutation == "prompt_chronology":
        rows[1]["messages"][0:2] = reversed(rows[1]["messages"][0:2])
    elif mutation == "output_pairing":
        # Rewrite both outputs from their first appearance, defeating a history-only comparison.
        rows[2]["messages"][2]["content"][0]["content"] = payloads[1]
        rows[2]["messages"][4]["content"][0]["content"] = payloads[0]
        rows = [rows[0], rows[2]]
    else:
        rows[1]["messages"][1:3] = reversed(rows[1]["messages"][1:3])
    with pytest.raises(AssertionError):
        check_scripted(
            rows, payloads, calls, history_counts=[0, 2] if mutation == "output_pairing" else None
        )
