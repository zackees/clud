"""Approved run dependencies must gate planning, independent of planner prose."""
from __future__ import annotations

import json

import pytest

from tests.harness.harness import Harness
from tests.harness.test_grind import (
    _by_role,
    _goal_roles,
    _goals,
    _merged,
    _no_notes,
    _script,
    _structured,
)


@pytest.mark.parametrize("upstream_succeeds", [True, False])
@pytest.mark.parametrize("mode", ["sequential", "parallel"])
@pytest.mark.parametrize("staged", [False, True])
def test_approved_merge_dependency_gates_planning(
    harness: Harness, upstream_succeeds: bool, mode: str, staged: bool,
) -> None:
    h = harness
    roles = [*_goal_roles(h, "a"), *_goal_roles(h, "b"), *_goal_roles(h, "c")]
    if not upstream_succeeds:
        planner = next(role for role in roles if role["name"] == "planner:a")
        planner["steps"] = [_structured({
            "checkout": str(h.repo), "branch": "grind/a", "depends_on": [],
            "verify": "true", "tasks": [],
        })]
    # Caller order is deliberately wrong, and every per-goal planner omits
    # depends_on. The approved graph must still control the execution order.
    graph = {
        "session_repo": "o/r",
        "session_root": str(h.repo),
        "goals": [
            {"id": "b", "repo": "o/r", "main": "main", "checkout": "local",
             "blockers": [{"goal": "a", "when": "merged"}]},
            {"id": "a", "repo": "o/r", "main": "main", "checkout": "local", "blockers": []},
            {"id": "c", "repo": "o/r", "main": "main", "checkout": "local", "blockers": []},
        ],
        "proposed_goals": [], "risks": [],
    }
    stage_args = {} if not staged else {
        "plan_url": "https://github.com/o/r/issues/100#issuecomment-1000",
        "plan": {"schema": "grind-plan/v1", "meta": 100, "stages": [
            {"stage": "bugs", "base": "main", "children": ["b", "c"]},
            {"stage": "feature", "group": "upstream", "base": "main", "children": ["a"]},
        ]},
    }
    result = h.run(
        "Execute the approved graph; do not infer dependencies from planner output.",
        _script(h, mode, _goals("b", "a", "c"), roles, orchestration=graph, **stage_args),
        timeout=300,
    )
    assert result.returncode == 0, result.stderr + result.stdout[-3000:]
    _no_notes(result)
    ran = _by_role(result)
    if upstream_succeeds:
        assert max(ran["lander:a:0"]) < min(ran["planner:b"])
        assert _merged(h) == ["grind/a", "grind/b", "grind/c"]
        downstream = json.dumps(result.first_request("planner:b")["messages"])
        assert "Upstream results (approved blockers):" in downstream
        state = h.read_gh_state()
        upstream_pr = next(pr for pr in state["prs"] if pr["head"] == "grind/a")
        assert f"https://github.com/o/r/pull/{upstream_pr['number']}" in downstream
        independent = json.dumps(result.first_request("planner:c")["messages"])
        assert "Upstream results (approved blockers):" not in independent
    else:
        assert not any(role.endswith(":b") or role.startswith("lander:b:") for role in ran)
        assert not (h.repo / "b.txt").exists()
        assert _merged(h) == ["grind/c"]
