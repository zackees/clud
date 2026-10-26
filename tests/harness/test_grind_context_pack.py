"""`/grind`'s context pack on the real Claude Code (#1339, #1344).

The planner returns the goal's issue text and code excerpts once; the
workflow puts that pack first in every worker's and the reviewer's prompt.
#1344's premise is that the workers' requests are then byte-identical up to
the end of the pack, so the prompt cache serves it after the first worker.
If Claude Code put anything agent-specific ahead of the pack, the first test
here fails and the caching design stops there.
"""

from __future__ import annotations

import json
from typing import Any

from tests.harness.harness import Harness, RunResult
from tests.harness.test_grind import _after, _goal_roles, _goals, _role, _script, _structured

PACK_END = "\n--- end of context pack ---\n"
TASKS = ("t1", "t2", "t3")
# Large enough to pass the workflow's staggered-start threshold.
EXCERPT = "\n".join(f"line {n}: let shared_context_{n} = {n};" for n in range(1, 160))


def _file(goal: str, task: str) -> str:
    """t1 writes the file the inherited integrator script commits."""
    return f"{goal}.txt" if task == "t1" else f"{goal}-{task}.txt"


def _pack_goal(h: Harness, goal: str) -> list[dict[str, Any]]:
    """One goal whose plan carries a pack and three parallel tasks."""
    roles = _goal_roles(h, goal)
    plan = {
        "checkout": str(h.repo),
        "branch": f"grind/{goal}",
        "depends_on": [],
        "verify": "true",
        "tasks": [
            {"id": t, "files": [_file(goal, t)], "instructions": f"write {_file(goal, t)}"}
            for t in TASKS
        ],
        "context": {
            "issue": "Issue body: make the three files.\nComment: keep them short.",
            "excerpts": [
                {"path": "src/shared.rs", "start": 1, "end": 159, "why": "shared", "text": EXCERPT}
            ],
        },
    }
    at = f"Goal {goal}:"
    ok = {"is_error": False}
    workers = []
    for t in TASKS:
        write = {"file_path": f"{h.repo}/{_file(goal, t)}", "content": f"{t}\n"}
        steps = [
            {"tool_use": {"name": "Write", "input": write}, "delay_ms": 300},
            _after(ok, _structured({"files_touched": [_file(goal, t)], "summary": t})),
        ]
        workers.append(_role(f"worker:{goal}:{t}", "worker", [at, f"Task {t}."], steps))
    rest = [r for r in roles if not r["name"].startswith(("planner:", "worker:"))]
    planner = _role(f"planner:{goal}", "planner", at, [_structured(plan)])
    return [planner, *workers, *rest]


def _first(result: RunResult, role: str) -> dict[str, Any]:
    return next(r for r in result.requests if r["role"] == role)


def _prefix(request: dict[str, Any]) -> str:
    """Everything the model sees, serialized, up to the end of the pack."""
    whole = json.dumps({"system": request.get("system"), "messages": request.get("messages")})
    marker = json.dumps(PACK_END)[1:-1]
    assert marker in whole, "the pack never reached this agent"
    return whole[: whole.index(marker) + len(marker)]


def _run(harness: Harness) -> RunResult:
    result = harness.run(
        "start grind",
        _script(harness, "parallel", _goals("g"), _pack_goal(harness, "g")),
        timeout=600,
    )
    assert result.returncode == 0, result.stdout[-3000:]
    return result


def test_workers_are_byte_identical_up_to_the_end_of_the_pack(harness: Harness) -> None:
    result = _run(harness)
    prefixes = {t: _prefix(_first(result, f"worker:g:{t}")) for t in TASKS}
    assert len(set(prefixes.values())) == 1, (
        "worker requests differ before the pack ends; the shared prefix cannot cache"
    )
    assert "make the three files" in prefixes["t1"]
    assert "shared_context_159" in prefixes["t1"]
    for t in TASKS:
        assert f"Task {t}." not in prefixes[t], "task-specific text precedes the pack"


def test_the_reviewer_starts_from_the_same_pack(harness: Harness) -> None:
    result = _run(harness)
    reviewer = json.dumps(_first(result, "reviewer:g").get("messages"))
    pack = json.dumps(_first(result, "worker:g:t1").get("messages"))
    start = pack.index("Context pack for this goal")
    end = pack.index(json.dumps(PACK_END)[1:-1]) + len(json.dumps(PACK_END)[1:-1])
    assert pack[start:end] in reviewer


def test_the_first_worker_runs_before_the_rest_start(harness: Harness) -> None:
    result = _run(harness)
    first = [r for r in result.requests if r["role"] == "worker:g:t1"]
    others = [r for r in result.requests if r["role"] in ("worker:g:t2", "worker:g:t3")]
    assert first
    assert others
    assert min(r["t_start"] for r in others) >= max(r["t_end"] for r in first), (
        "the other workers started before the first could write the cache"
    )
