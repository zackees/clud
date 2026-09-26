"""Questions happen up front, never after prework, on the real Claude Code (#1407).

Covers #1392 U11: a `grind-*` sub-agent that calls `AskUserQuestion` is
denied by the clud hook and told to have questions asked up front. Also covers
the ordering invariant U10: the main session asks its one question round
before the grind-run workflow starts, and nothing is asked after the first
`grind-planner` record.

Script note: a step's `expect` checks the tool results of the *previous*
step, so an expectation about a call sits on the step after it.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from tests.harness.harness import Harness
from tests.harness.worlds import _issue, _world

MARK = {"planner": "You are the /grind planner"}
META = "100"

QUESTION = {
    "question": "Which?",
    "header": "Pick",
    "multiSelect": False,
    "options": [
        {"label": "A", "description": "a"},
        {"label": "B", "description": "b"},
    ],
}
ASK = {"tool_use": {"name": "AskUserQuestion", "input": {"questions": [QUESTION]}}}


def _structured(value: dict[str, Any]) -> dict[str, Any]:
    return {"tool_use": {"name": "StructuredOutput", "input": value}}


def _after(expect: dict[str, Any], step: dict[str, Any]) -> dict[str, Any]:
    """`step`, first checking the previous step's tool results."""
    return {**step, "expect": expect}


def _role(
    name: str, role: str, prompt: str | list[str], steps: list[dict[str, Any]]
) -> dict[str, Any]:
    return {"name": name, "match": MARK[role], "match_prompt": prompt, "steps": steps}


def _seed(h: Harness, children: list[int]) -> dict[str, Any]:
    """Meta #100 with `children` as native sub-issues; returns the state."""
    issues = {int(META): _issue("meta: backlog", "Tracked as sub-issues.")}
    for n in children:
        issues[n] = _issue(f"child {n}", f"do {n}", parent=int(META))
    state = _world(issues)
    h.write_gh_state(state)
    return h.read_gh_state()


def _plan_phase(h: Harness) -> None:
    run = Path(h.repo) / ".clud" / "grind" / "run.json"
    run.parent.mkdir(parents=True, exist_ok=True)
    run.write_text(json.dumps({"phase": "plan"}), encoding="utf-8")


def _classify(children: list[int]) -> dict[str, Any]:
    """A minimal plan: every child is an independent bug on main."""
    return {
        "children": [{"id": str(n), "track": "bug", "depends_on_bugs": []} for n in children],
        "groups": [],
        "order": [str(n) for n in children],
        "confident": True,
    }


def _script(
    h: Harness,
    children: list[int],
    planner_steps: list[dict[str, Any]],
    main_prefix: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    goals = [{"id": str(n), "title": f"child {n}", "brief": f"do {n}"} for n in children]
    args = {
        "repo": str(h.repo),
        "main": "main",
        "mode": "sequential",
        "meta": META,
        "planOnly": True,
        "goals": goals,
    }
    main = {
        "name": "main",
        "steps": [
            *(main_prefix or []),
            {"tool_use": {"name": "Workflow", "input": {"name": "grind-run", "args": args}}},
            {"text": "PLANNED"},
        ],
    }
    return {
        "default_text": "OK",
        "roles": [_role("planner", "planner", "PLAN-ONLY", planner_steps), main],
    }


def test_grind_subagent_ask_user_question_is_denied(harness: Harness) -> None:
    children = [101]
    steps = [ASK, _after({"is_error": True}, _structured(_classify(children)))]
    _seed(harness, children)
    _plan_phase(harness)
    result = harness.run("start grind", _script(harness, children, steps), timeout=300)
    assert result.returncode == 0, result.stdout[-3000:]
    asked = [
        h
        for h in result.hooks
        if h.get("agent_type") == "grind-planner"
        and h.get("event") == "PreToolUse"
        and h.get("tool_name") == "AskUserQuestion"
    ]
    assert asked, result.hooks
    planner = json.dumps([r["messages"] for r in result.requests if r["role"] == "planner"])
    assert "up front" in planner, planner[-3000:]


def test_main_session_asks_before_the_workflow(harness: Harness) -> None:
    children = [101]
    _seed(harness, children)
    _plan_phase(harness)
    script = _script(harness, children, [_structured(_classify(children))], main_prefix=[ASK])
    result = harness.run("start grind", script, timeout=300, answers={"*": "A"})
    assert result.returncode == 0, result.stdout[-3000:]
    assert len(result.questions_before("grind-planner")) == 1, result.hooks
    seen_planner = False
    for record in result.hooks:
        if record.get("agent_type") == "grind-planner":
            seen_planner = True
        if seen_planner:
            assert record.get("tool_name") != "AskUserQuestion", record
