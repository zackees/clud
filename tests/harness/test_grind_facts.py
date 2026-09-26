"""`/grind` run facts are per session, not per repo (#1337).

The router records its run facts in `~/.clud/tmp/grind/<session_id>.json`,
found through `clud grind-facts path`, and clud's hook reads the file for the
`session_id` in each PreToolUse payload. These tests pin the three facts that
design rests on, on the real Claude Code:

* a workflow agent's PreToolUse payload carries its parent session's id;
* `CLAUDE_CODE_SESSION_ID` in the router's shell is that same id, so
  `clud grind-facts path` names the file the hook reads;
* another session's facts never apply: each session's planner is capped by
  its own run's facts, and one run clearing its file leaves the other alone.

The planner is driven through `grind-run`'s plan-only pass (as in
`test_grind_plan_only.py`), whose `git worktree add` the caps allow only in
parallel mode and refuse in plan-only mode.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from tests.harness.harness import Harness, RunResult
from tests.harness.test_grind_plan_only import (
    _after,
    _bash,
    _classify,
    _no_notes,
    _script,
    _seed,
    _structured,
)

DENIED = {"is_error": True}


def _planner_bash(result: RunResult) -> list[dict[str, Any]]:
    return [
        h
        for h in result.hooks
        if h.get("agent_type") == "grind-planner"
        and h.get("event") == "PreToolUse"
        and h.get("tool_name") == "Bash"
    ]


def _planner_results(result: RunResult) -> str:
    return json.dumps([r["messages"] for r in result.requests if r["role"] == "planner"])


def _worktree_add(h: Harness, name: str) -> tuple[str, str]:
    path = f"{h.repo}-{name}"
    return path, f"git -C {h.repo} worktree add {path} -b grind/{name} origin/main"


def _run_planner(h: Harness, add: str, expect_after_add: dict[str, Any]) -> RunResult:
    plan = _classify({101: ("bug", None)}, {})
    steps = [_bash(add), _after(expect_after_add, _structured(plan))]
    result = h.run("start grind", _script(h, [101], steps), timeout=300)
    assert result.returncode == 0, result.stdout[-3000:]
    _no_notes(result)
    return result


def test_workflow_agents_carry_the_parent_session_id(harness: Harness) -> None:
    h = harness
    _seed(h, [101])
    h.write_run_facts({"phase": "plan"})
    _, add = _worktree_add(h, "a")
    result = _run_planner(h, add, {**DENIED, "content_contains": "plan-only"})
    ids = {r.get("session_id") for r in result.hooks}
    assert ids == {h.session_id}, ids
    assert _planner_bash(result), result.hooks


def test_grind_facts_path_names_the_file_the_hook_reads(harness: Harness) -> None:
    h = harness
    command = "clud grind-facts path"
    script = {
        "default_text": "OK",
        "roles": [{"name": "main", "steps": [_bash(command), {"text": "DONE"}]}],
    }
    result = h.run("where are the facts?", script, timeout=120)
    assert result.returncode == 0, result.stdout[-3000:]
    printed = [
        e["tool_use_result"]["stdout"]
        for e in result.events
        if e.get("type") == "user" and isinstance(e.get("tool_use_result"), dict)
    ]
    assert printed == [str(h.run_facts_path())], printed
    assert h.run_facts_path().parent.is_dir()


def test_each_session_is_capped_by_its_own_runs_facts(harness: Harness) -> None:
    h = harness
    _seed(h, [101])
    other = h.home / ".clud" / "tmp" / "grind" / "00000000-0000-0000-0000-000000000000.json"

    # Session 1: its own run is parallel, so its planner may add a worktree,
    # though another session's run is in plan-only mode.
    h.write_run_facts({"mode": "parallel"})
    other.write_text(json.dumps({"phase": "plan"}), encoding="utf-8")
    path, add = _worktree_add(h, "one")
    first = _run_planner(h, add, {"is_error": False})
    assert [c["tool_input"]["command"] for c in _planner_bash(first)] == [add]
    assert Path(path).is_dir(), path

    # Session 2 has no facts of its own; session 1's parallel facts and the
    # other session's must not leak in, so the strictest caps apply.
    h.new_session()
    _, add2 = _worktree_add(h, "two")
    second = _run_planner(h, add2, {**DENIED, "content_contains": "only in parallel mode"})
    assert [c["tool_input"]["command"] for c in _planner_bash(second)] == [add2]

    # The other session clearing its file leaves this session's facts alone.
    h.new_session()
    h.write_run_facts({"phase": "plan"})
    other.unlink()
    _, add3 = _worktree_add(h, "three")
    third = _run_planner(h, add3, {**DENIED, "content_contains": "plan-only"})
    assert '"is_error": true' in _planner_results(third)
