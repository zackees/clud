"""Bounded native Workflow gate attempts must observe publication, then defer."""
from __future__ import annotations

import pytest

from tests.harness.harness import Harness
from tests.harness.test_grind import (
    _after,
    _bash,
    _by_role,
    _goal_roles,
    _goals,
    _merged,
    _no_notes,
    _script,
    _structured,
)
from tests.harness.test_grind_gate_binding import install_outcome_guard
from tests.harness.workflow_results import workflow_results


@pytest.mark.parametrize("opens_after", [2, None])
def test_native_publication_gate_uses_fresh_bounded_attempts_with_backoff(
    harness: Harness, monkeypatch: pytest.MonkeyPatch, opens_after: int | None,
) -> None:
    h = harness
    command = "gh release view v0.9.0 --repo o/r"
    graph = {"session_repo": "o/r", "session_root": str(h.repo), "goals": [
        {"id": "a", "repo": "o/r", "main": "main", "blockers": []},
        {"id": "b", "repo": "o/r", "main": "main", "blockers": [
            {"goal": "a", "when": "released", "check": command},
        ]},
        {"id": "c", "repo": "o/r", "main": "main", "blockers": []},
    ], "proposed_goals": [], "risks": []}
    if opens_after is not None:
        state = h.read_gh_state()
        state["release_after_views"] = {"v0.9.0": {
            "after": opens_after, "release": {"publishedAt": "2026-10-22T00:00:00Z"},
        }}
        h.write_gh_state(state)
    h.write_run_facts({"phase": "execute", "orchestration": graph})
    install_outcome_guard(h, monkeypatch)
    gates = []
    for attempt in (2, 3, 1):
        ready = attempt == 3 and opens_after is not None
        role = {"name": f"gate:{attempt}", "match": "You are the /grind gate checker", "steps": [
            _bash(command),
            _after({"is_error": not ready}, _structured({
                "ready": ready, "evidence": "release available" if ready else "release absent",
            })),
        ]}
        if attempt != 1:
            role["match_prompt"] = f"Observation: 0/{attempt}."
        gates.append(role)
    roles = [*_goal_roles(h, "a"), *_goal_roles(h, "b"), *_goal_roles(h, "c"), *gates]
    result = h.run("Use bounded native gate attempts; keep independent work moving.",
                   _script(h, "parallel", _goals("a", "b", "c"), roles,
                           orchestration=graph, gateAttempts=3, gateBackoffMs=200), timeout=300)
    assert result.returncode == 0, result.stderr + result.stdout[-3000:]
    _no_notes(result)
    ran = _by_role(result)
    assert all(f"gate:{attempt}" in ran for attempt in (1, 2, 3)), ran
    calls = [row for row in result.hooks if row.get("agent_type") == "grind-gate"
             and row.get("event") == "PreToolUse" and row.get("tool_name") == "Bash"]
    assert [row["tool_input"]["command"] for row in calls] == [command] * 3, calls
    assert len({row["agent_id"] for row in calls}) == 3, calls
    for attempt, minimum in ((2, 200), (3, 400)):
        previous = [row for row in result.requests if row["role"] == f"gate:{attempt - 1}"]
        current = [row for row in result.requests if row["role"] == f"gate:{attempt}"]
        assert min(row["t_start"] for row in current) - max(
            row["t_end"] for row in previous
        ) >= minimum
    assert min(ran["planner:c"]) < min(ran["gate:3"])
    if opens_after is None:
        assert "planner:b" not in ran
        assert _merged(h) == ["grind/a", "grind/c"]
        outputs = workflow_results(result)
        assert len(outputs) == 1, outputs
        assert outputs[0]["resume"] == {
            "schema": "grind-publication-resume/v1", "remaining_goals": ["b"],
            "gateEpochs": {"b": 1},
        }, outputs
    else:
        assert max(ran["gate:3"]) < min(ran["planner:b"])
        assert _merged(h) == ["grind/a", "grind/b", "grind/c"]
        assert "resume" not in workflow_results(result)[0]
