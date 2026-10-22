"""Run-plan validation on the real pinned Workflow runtime (#1338, V1-V6).

These cases deliberately use plan-only mode: invalid orchestration input must
be rejected before even the read-only classification planner is started.
Filesystem checkout safety and repository hook context need separate probes.
"""

from __future__ import annotations

import copy
from typing import Any

import pytest

from tests.harness.harness import Harness
from tests.harness.test_grind_plan_only import (
    _classify,
    _plan_phase,
    _script,
    _seed,
    _structured,
    _told,
)


def run_plan() -> dict[str, Any]:
    return {
        "session_repo": "FastLED/FastLED",
        "goals": [
            {"id": "reld-208", "repo": "zackees/reld", "main": "main",
             "checkout": "worktree", "blockers": []},
            {"id": "reld-209", "repo": "zackees/reld", "main": "main",
             "blockers": [{"goal": "reld-208", "when": "merged"}]},
            {"id": "4642", "repo": "FastLED/FastLED", "main": "master",
             "checkout": "local", "blockers": [
                 {"goal": "reld-209", "when": "released",
                  "check": "gh release view --repo zackees/reld v0.9.0"}]},
            {"id": "4641", "repo": "FastLED/FastLED", "main": "master",
             "blockers": [{"goal": "4642", "when": "merged"}]},
        ],
        "proposed_goals": [],
        "risks": [],
    }


def invalid_plan(case: str) -> tuple[dict[str, Any], str]:
    plan = copy.deepcopy(run_plan())
    goals = plan["goals"]
    if case == "cycle":
        goals[2]["blockers"] = [{"goal": "4641", "when": "merged"}]
        reason = "dependency cycle: 4642 → 4641 → 4642"
    elif case == "unknown-goal":
        goals[2]["blockers"] = [{"goal": "reld-999", "when": "merged"}]
        reason = 'unknown goal "reld-999" in blockers of 4642'
    elif case == "missing-check":
        del goals[2]["blockers"][0]["check"]
        reason = "blocker reld-209 → 4642 (released) has no check"
    elif case == "unknown-condition":
        goals[2]["blockers"][0]["when"] = "shipped"
        reason = 'unknown condition "shipped"'
    elif case == "missing-repo":
        del goals[2]["repo"]
        reason = "goal 4642: repo and main are required"
    elif case == "missing-main":
        del goals[2]["main"]
        reason = "goal 4642: repo and main are required"
    else:
        assert case == "sister-local"
        goals[0]["checkout"] = "local"
        reason = "goal reld-208: a sister repo must use a worktree"
    return plan, reason


@pytest.mark.parametrize("case", [
    "cycle", "unknown-goal", "missing-check", "unknown-condition",
    "missing-repo", "missing-main", "sister-local",
])
def test_invalid_orchestration_stops_before_planning(harness: Harness, case: str) -> None:
    plan, reason = invalid_plan(case)
    _seed(harness, [101])
    _plan_phase(harness)
    script = _script(harness, [101], [_structured(_classify({101: ("bug", None)}, {}))])
    script["roles"][-1]["steps"][0]["tool_use"]["input"]["args"]["orchestration"] = plan
    before = harness.git("status", "--porcelain")
    result = harness.run(
        "Validate the supplied run plan before starting any role.", script, timeout=120,
    )
    assert result.returncode == 0, result.stderr + result.stdout[-3000:]
    roles = {row["role"] for row in result.requests}
    assert roles == {"main"}, {"unexpected_roles": sorted(roles - {"main"})}
    # JSON encodes quotes inside the workflow error; decode them for diagnostics.
    assert reason in _told(result).replace('\\"', '"'), _told(result)[-4000:]
    assert harness.git("status", "--porcelain") == before
    assert harness.read_gh_state()["issues"]["101"]["state"] == "open"
