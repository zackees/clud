"""`/grind` input routing on the real Claude Code (#1405, cases R1-R11 of #1392).

The main session plays `/grind` intake only: the scripted model runs the
Bash calls the router's intake step allows (`is_meta_issue.py`, `gh issue
create`, `gh api ... sub_issues`, `gh issue comment`) and asks the convert
question via `AskUserQuestion`. The model's decisions are scripted, so these
tests mainly guard that the hook caps, the bundled tool and the fake GitHub
semantics support each routing flow, and that refusals leave GitHub alone.

Script note: a step's `expect` checks the tool results of the *previous*
step, so an expectation about a command sits on the step after it.

Fake-GitHub note: `gh repo view` and `gh api repos/o/r/issues/N` are not
modelled by `fake_gh` (they print nothing / `{}`), so the tool is always
given the repo (a URL or `--repo o/r`), and task-list detection is read
with `gh issue view --json body` rather than through the tool.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.harness import worlds
from tests.harness.harness import Harness, RunResult, questions_before

TOOL = '"$CLUD_EXE" tool run github/is_meta_issue.py'
OK = {"is_error": False}
CONVERT_Q = {
    "question": "Issue #1 has several independent parts. Convert it to a meta issue?",
    "header": "Convert",
    "multiSelect": False,
    "options": [
        {"label": "Convert", "description": "split into sub-issues under a new meta"},
        {"label": "Don't convert", "description": "stop; run /do on it instead"},
    ],
}


def _structured(value: dict[str, Any]) -> dict[str, Any]:
    return {"tool_use": {"name": "StructuredOutput", "input": value}}


def _bash(command: str) -> dict[str, Any]:
    return {"tool_use": {"name": "Bash", "input": {"command": command, "description": "grind"}}}


def _after(expect: dict[str, Any], step: dict[str, Any]) -> dict[str, Any]:
    """`step`, first checking the previous step's tool results."""
    return {**step, "expect": expect}


def _ask() -> dict[str, Any]:
    return {"tool_use": {"name": "AskUserQuestion", "input": {"questions": [CONVERT_Q]}}}


def _main(steps: list[dict[str, Any]]) -> dict[str, Any]:
    return {"default_text": "OK", "roles": [{"name": "main", "steps": steps}]}


def _issues(h: Harness) -> dict[str, Any]:
    return h.read_gh_state().get("issues", {})


def _no_notes(result: RunResult) -> None:
    notes = [(r["role"], r["note"]) for r in result.requests if r.get("note")]
    assert not notes, notes


def _no_workflow(result: RunResult) -> None:
    assert [r["role"] for r in result.requests if r["role"] != "main"] == []
    grind = [h for h in result.hooks if str(h.get("agent_type") or "").startswith("grind-")]
    assert grind == []
    assert not [h for h in result.hooks if h.get("tool_name") == "Workflow"]


def _asked(result: RunResult) -> int:
    return len(questions_before(result, "grind-planner"))


def _created(h: Harness, before: dict[str, Any]) -> list[str]:
    return sorted((n for n in _issues(h) if n not in before), key=int)


def _worktrees(h: Harness) -> int:
    return len(h.git("worktree", "list").splitlines())


def _attach(parent: str, child: str) -> str:
    return f"gh api -X POST repos/o/r/issues/{parent}/sub_issues -F sub_issue_id={child}"


def _run(h: Harness, steps: list[dict[str, Any]], **kw: Any) -> RunResult:
    result = h.run("/grind", _main(steps), **kw)
    assert result.returncode == 0, result.stdout[-3000:]
    _no_notes(result)
    return result


def test_r1_prompt_with_three_deliverables_becomes_a_meta_with_three_children(
    harness: Harness,
) -> None:
    before = _issues(harness)
    steps = [
        _bash("gh issue create --title 'add --verbose' --body 'add a --verbose flag'"),
        _after(OK, _bash("gh issue create --title 'log steps' --body 'log each step'")),
        _after(OK, _bash("gh issue create --title 'document' --body 'document the flag'")),
        _after(OK, _bash("gh issue create --title 'meta: verbose' --body 'Tracks the prompt'")),
        _after(OK, _bash(" && ".join(_attach("4", c) for c in ("1", "2", "3")))),
        _after(OK, {"text": "META_READY #4"}),
    ]
    result = _run(harness, steps)
    assert _created(harness, before) == ["1", "2", "3", "4"]
    issues = _issues(harness)
    assert [s["number"] for s in issues["4"]["sub_issues"]] == [1, 2, 3]
    assert all(issues[c]["parent"] == 4 for c in ("1", "2", "3"))
    calls = harness.read_gh_state()["calls"]
    assert sum(1 for c in calls if c[:1] == ["api"] and "sub_issues" in c[-3]) == 3
    _no_workflow(result)


def test_r2_issue_list_gets_one_meta_and_nothing_else(harness: Harness) -> None:
    state = worlds._world({n: worlds._issue(f"task {n}", f"do {n}") for n in (7, 8, 9)})
    harness.write_gh_state(state)
    before = _issues(harness)
    steps = [
        _bash("gh issue create --title 'meta: 7 8 9' --body 'Tracks #7 #8 #9'"),
        _after(OK, _bash(" && ".join(_attach("10", c) for c in ("7", "8", "9")))),
        _after(OK, {"text": "META_READY #10"}),
    ]
    result = _run(harness, steps)
    assert _created(harness, before) == ["10"]
    issues = _issues(harness)
    assert [s["number"] for s in issues["10"]["sub_issues"]] == [7, 8, 9]
    _no_workflow(result)


def test_r3_native_sub_issue_meta_is_used_as_is(harness: Harness) -> None:
    harness.write_gh_state(worlds.meta_with_sub_issues())
    before = _issues(harness)
    steps = [
        _bash(f"{TOOL} 1 --repo o/r"),
        _after({"is_error": False, "content_contains": '"meta": true'}, {"text": "IS_META"}),
    ]
    result = _run(harness, steps)
    assert "IS_META" in result.stdout
    assert _asked(result) == 0
    assert _issues(harness) == before


def test_r4_task_list_meta_is_used_as_is(harness: Harness) -> None:
    harness.write_gh_state(worlds.task_list_meta())
    before = _issues(harness)
    steps = [
        _bash("gh issue view 1 --json body"),
        _after({"is_error": False, "content_contains": "- [ ] #2"}, {"text": "IS_META"}),
    ]
    result = _run(harness, steps)
    assert "IS_META" in result.stdout
    assert _asked(result) == 0
    assert _issues(harness) == before


def test_r5_multi_part_issue_converted_on_consent(harness: Harness) -> None:
    harness.write_gh_state(worlds.multi_part())
    before = _issues(harness)
    create = "gh issue create --title '{t}' --body '> From #1: {t}'"
    steps = [
        _bash(f"{TOOL} 1 --repo o/r"),
        _after({"is_error": False, "content_contains": '"meta": false'}, _ask()),
        _after(
            {"is_error": False, "content_contains": "Convert"},
            _bash(create.format(t="add a --verbose flag")),
        ),
        _after(OK, _bash(create.format(t="log each step when verbose"))),
        _after(OK, _bash(create.format(t="document the flag in README"))),
        _after(OK, _bash("gh issue create --title 'meta: verbose mode' --body 'Tracks #1'")),
        _after(OK, _bash(" && ".join(_attach("5", c) for c in ("2", "3", "4")))),
        _after(OK, _bash("gh issue comment 1 --body 'Split into meta #5'")),
        _after(OK, {"text": "CONVERTED #5"}),
    ]
    result = _run(harness, steps, answers={"*": "Convert"})
    assert _asked(result) == 1
    assert _created(harness, before) == ["2", "3", "4", "5"]
    issues = _issues(harness)
    assert all(">" in issues[c]["body"] for c in ("2", "3", "4"))
    assert "Tracks #1" in issues["5"]["body"]
    assert [s["number"] for s in issues["5"]["sub_issues"]] == [2, 3, 4]
    assert any("Split into meta" in c["body"] for c in issues["1"]["comments"])
    _no_workflow(result)


def test_r6_multi_part_issue_left_alone_on_refusal(harness: Harness) -> None:
    harness.write_gh_state(worlds.multi_part())
    before = _issues(harness)
    trees = _worktrees(harness)
    steps = [
        _bash(f"{TOOL} 1 --repo o/r"),
        _after({"is_error": False, "content_contains": '"meta": false'}, _ask()),
        _after(
            {"is_error": False, "content_contains": "Don't convert"},
            {"text": "Not converting. Run `/do https://github.com/o/r/issues/1` instead."},
        ),
    ]
    result = _run(harness, steps, answers={"*": "Don't convert"})
    assert "/do" in result.stdout
    assert _asked(result) == 1
    assert _issues(harness) == before
    assert _worktrees(harness) == trees
    _no_workflow(result)


def test_r7_single_change_issue_is_sent_to_do_without_asking(harness: Harness) -> None:
    harness.write_gh_state(worlds.single_change())
    before = _issues(harness)
    steps = [
        _bash(f"{TOOL} 1 --repo o/r"),
        _after(
            {"is_error": False, "content_contains": '"meta": false'},
            {"text": "Single change: use `/do https://github.com/o/r/issues/1`."},
        ),
    ]
    result = _run(harness, steps)
    assert "/do" in result.stdout
    assert _asked(result) == 0
    assert _issues(harness) == before
    _no_workflow(result)


def test_r8_meta_with_every_sub_issue_closed_has_nothing_to_do(harness: Harness) -> None:
    state = worlds.meta_with_sub_issues()
    for n in ("2", "3", "4"):
        state["issues"][n]["state"] = "closed"
    for s in state["issues"]["1"]["sub_issues"]:
        s["state"] = "closed"
    harness.write_gh_state(state)
    before = _issues(harness)
    steps = [
        _bash(f"{TOOL} 1 --repo o/r"),
        _after(
            {"is_error": False, "content_contains": '"state": "closed"'},
            {"text": "Nothing to do: every sub-issue of #1 is closed."},
        ),
    ]
    result = _run(harness, steps)
    assert "Nothing to do" in result.stdout
    assert _issues(harness) == before
    _no_workflow(result)


def test_r9_sub_issues_fault_surfaces_and_changes_nothing(harness: Harness) -> None:
    state = worlds.meta_with_sub_issues()
    state["faults"] = {"api GET": {"code": 1, "stderr": "gh: Server Error (HTTP 502)"}}
    harness.write_gh_state(state)
    before = _issues(harness)
    steps = [
        _bash(f"{TOOL} 1 --repo o/r; echo EXIT=$?"),
        _after(
            {"is_error": False, "content_contains": "EXIT=2"},
            {"text": "is_meta_issue.py failed (exit 2): HTTP 502"},
        ),
    ]
    result = _run(harness, steps)
    assert "HTTP 502" in result.stdout
    assert _issues(harness) == before
    _no_workflow(result)


@pytest.mark.parametrize("target", ["1 --repo o/r", "https://github.com/o/r/issues/1"])
def test_r10_url_and_bare_number_route_the_same(harness: Harness, target: str) -> None:
    harness.write_gh_state(worlds.meta_with_sub_issues())
    before = _issues(harness)
    steps = [
        _bash(f"{TOOL} {target}"),
        _after({"is_error": False, "content_contains": '"meta": true'}, {"text": "IS_META"}),
    ]
    result = _run(harness, steps)
    assert "IS_META" in result.stdout
    assert _asked(result) == 0
    assert _issues(harness) == before
    sub_calls = [c for c in harness.read_gh_state()["calls"] if c[:1] == ["api"]]
    assert sub_calls[0][1] == "repos/o/r/issues/1/sub_issues"


def test_r11_second_child_create_fails_and_no_workflow_starts(harness: Harness) -> None:
    harness.write_gh_state(worlds.multi_part())
    before = _issues(harness)
    # fake_gh faults fire on the first matching call, so the first child is
    # created in one session and the fault is armed before the second.
    first = [
        _bash("gh issue create --title 'add a --verbose flag' --body '> From #1'"),
        _after(OK, {"text": "CHILD_1"}),
    ]
    one = _run(harness, first)
    state = harness.read_gh_state()
    state["faults"] = {"issue create": {"code": 1, "stderr": "gh: Server Error (HTTP 502)"}}
    harness.write_gh_state(state)
    second = [
        _bash("gh issue create --title 'log each step' --body '> From #1'; echo EXIT=$?"),
        _after(
            {"is_error": False, "content_contains": "EXIT=1"},
            {"text": "Conversion failed at child 2 (HTTP 502). Partial state: created #2."},
        ),
    ]
    two = _run(harness, second)
    assert "Partial state: created #2" in two.stdout
    assert _created(harness, before) == ["2"]
    assert _issues(harness)["1"]["comments"] == []
    for result in (one, two):
        _no_workflow(result)
