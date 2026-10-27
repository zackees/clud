"""Negative controls for repeated-context and request-growth detection (#2225)."""

from __future__ import annotations

import copy

import pytest

from tests.harness.context_audit import audit_context


def fixture_requests() -> list[dict]:
    rows = []
    for turn in range(3):
        results = [{"type": "tool_result", "tool_use_id": f"tool-{i}", "content": f"OUTPUT_{i}"}
                   for i in range(turn)]
        rows.append({"system": "INSTRUCTION_ONCE", "messages": [{"role": "user", "content": results}],
                     "context_bytes": {"request": 1000 + 100 * turn, "system": 20,
                                       "tools": 50, "messages": 50 + 100 * turn,
                                       "tools_fingerprint": "stable"}})
    return rows


def check(rows: list[dict]) -> dict:
    return audit_context(rows, instruction_marker="INSTRUCTION_ONCE",
                         output_markers=["OUTPUT_0", "OUTPUT_1"],
                         initial_budget=1200, growth_budget=200)


def test_openrouter_context_audit_accepts_append_only_history() -> None:
    report = check(fixture_requests())
    assert report["request_count"] == 3
    assert report["cumulative_request_bytes"] == 3300


@pytest.mark.parametrize("mutation", ["instructions", "tool_result", "growth", "request", "schema"])
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
    else:
        rows[1]["context_bytes"]["tools_fingerprint"] = "changed"
    with pytest.raises(AssertionError):
        check(rows)

