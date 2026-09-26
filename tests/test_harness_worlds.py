"""Unit tests for the /grind harness helpers (#1402): worlds, answer hook,
and `questions_before`. No Claude Code needed."""

from __future__ import annotations

import io
import json
import sys
from pathlib import Path

import pytest

from tests.harness import answer_hook, worlds
from tests.harness.harness import RunResult, questions_before


@pytest.mark.parametrize("name", sorted(worlds.ALL))
def test_world_is_consistent(name: str) -> None:
    state = worlds.ALL[name]()
    issues = state["issues"]
    assert state["repo"] == "o/r"
    assert state["default_branch"] == "main"
    assert len(set(issues)) == len(issues)
    assert all(int(k) < state["next_id"] for k in issues)
    for key, issue in issues.items():
        for sub in issue["sub_issues"]:
            assert issues[str(sub["number"])]["parent"] == int(key)
        if issue["parent"] is not None:
            parent = issues[str(issue["parent"])]
            assert int(key) in [s["number"] for s in parent["sub_issues"]]


def test_grind_sub_meta_is_marked() -> None:
    issue = worlds.with_grind_sub_meta()["issues"]["3"]
    assert "grind:meta" in issue["labels"]
    assert "<!-- clud-grind -->" in issue["body"]
    user = worlds.with_user_sub_meta()["issues"]["3"]
    assert "grind:meta" not in user["labels"]


def _hook(monkeypatch: pytest.MonkeyPatch, tmp_path: Path, event: dict, table: dict) -> int:
    path = tmp_path / "answers.json"
    path.write_text(json.dumps(table), encoding="utf-8")
    monkeypatch.setattr(sys, "argv", ["answer_hook.py", str(path)])
    monkeypatch.setattr(sys, "stdin", io.StringIO(json.dumps(event)))
    return answer_hook.main()


def test_answer_hook_answers_ask_user_question(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    questions = [
        {"question": "Colour?", "header": "c", "options": [{"label": "red"}, {"label": "blue"}]},
        {"question": "Mode?", "header": "Mode", "options": [{"label": "a"}, {"label": "b"}]},
        {"question": "Size?", "header": "s", "options": [{"label": "big"}, {"label": "small"}]},
    ]
    event = {"tool_name": "AskUserQuestion", "tool_input": {"questions": questions}}
    assert _hook(monkeypatch, tmp_path, event, {"Colour?": "blue", "Mode": "b"}) == 0
    out = json.loads(capsys.readouterr().out)["hookSpecificOutput"]
    assert out["hookEventName"] == "PreToolUse"
    assert out["permissionDecision"] == "allow"
    assert out["updatedInput"]["questions"] == questions
    assert out["updatedInput"]["answers"] == {"Colour?": "blue", "Mode?": "b", "Size?": "big"}


def test_answer_hook_default_star(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    q = {"question": "Colour?", "options": [{"label": "red"}, {"label": "blue"}]}
    event = {"tool_name": "AskUserQuestion", "tool_input": {"questions": [q]}}
    _hook(monkeypatch, tmp_path, event, {"*": "blue"})
    out = json.loads(capsys.readouterr().out)
    assert out["hookSpecificOutput"]["updatedInput"]["answers"] == {"Colour?": "blue"}


def test_answer_hook_ignores_other_tools(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    event = {"tool_name": "Bash", "tool_input": {"command": "ls"}}
    assert _hook(monkeypatch, tmp_path, event, {"*": "x"}) == 0
    assert capsys.readouterr().out == ""


def _ask(agent_type: str | None = None) -> dict:
    return {"tool_name": "AskUserQuestion", "agent_type": agent_type}


def test_questions_before() -> None:
    hooks = [
        _ask(),
        {"tool_name": "Bash", "agent_type": None},
        _ask(),
        {"tool_name": "Read", "agent_type": "grind-planner"},
        _ask(),
    ]
    result = RunResult(returncode=0, stdout="", requests=[], hooks=hooks)
    assert len(questions_before(result, "grind-planner")) == 2
    assert len(result.questions_before("grind-planner")) == 2
    assert len(questions_before(result, "grind-lander")) == 3
    empty = RunResult(returncode=0, stdout="", requests=[], hooks=[])
    assert questions_before(empty, "grind-planner") == []
