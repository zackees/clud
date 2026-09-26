"""`/grind` input routing on the real Claude Code (#1405, cases R1-R11 of #1392).

The main session plays `/grind` intake only: the scripted model runs the
Bash calls the router's intake step allows (`is_meta_issue.py`, `gh issue
create`, `gh api ... sub_issues`, `gh issue comment`) and asks the convert
question via `AskUserQuestion`. The model's decisions are scripted, so these
tests mainly guard that the hook caps, the bundled tool and the fake GitHub
semantics support each routing flow, and that refusals leave GitHub alone.

Script notes:

- A step's `expect` checks the tool results of the *previous* step, so an
  expectation about a command sits on the step after it.
- The tool is invoked as `clud tool run ...`, the shape the intake skill
  uses. A `"$CLUD_EXE"` program word is refused by clud-cmd-scan's rm
  identity check ("command changes or bypasses provable shim resolution"),
  because a variable program word cannot be proven not to be `rm`.
- `fake_gh` returns an issue's REST shape (`id` equal to the number) for
  `gh api repos/o/r/issues/N` and its parent for `.../N/parent`, so the tool
  reads task-list bodies itself and both link directions can be verified.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from tests.harness import worlds
from tests.harness.harness import Harness, RunResult, questions_before

TOOL = "clud tool run github/is_meta_issue.py"
OK = {"is_error": False}
CONVERT = "Convert to a meta issue"
ABORT = "Abort"
CHILDREN = ("add a --verbose flag", "log each step when verbose", "document the flag in README")
CONVERT_Q = {
    "question": (
        "Convert #1 into a meta issue? Proposed children: "
        + "; ".join(f"{t} (one part of #1)" for t in CHILDREN)
    ),
    "header": "Convert",
    "multiSelect": False,
    "options": [
        {"label": CONVERT, "description": "split into sub-issues under a new meta issue"},
        {"label": ABORT, "description": "stop; run /do 1 on it instead"},
    ],
}
REFUSAL = (
    "`clud grind` works on meta issues. #1 is a single change; "
    "run `/do 1` (or `clud do 1`) instead."
)


def _bash(command: str) -> dict[str, Any]:
    return {"tool_use": {"name": "Bash", "input": {"command": command, "description": "grind"}}}


def _after(expect: dict[str, Any], step: dict[str, Any]) -> dict[str, Any]:
    """`step`, first checking the previous step's tool results."""
    return {**step, "expect": expect}


def _saw(text: str) -> dict[str, Any]:
    return {"is_error": False, "content_contains": text}


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


def _questions(result: RunResult) -> list[dict[str, Any]]:
    return questions_before(result, "grind-planner")


def _created(h: Harness, before: dict[str, Any]) -> list[str]:
    return sorted((n for n in _issues(h) if n not in before), key=int)


def _refs(h: Harness, where: Path | None = None) -> str:
    return h.git("for-each-ref", "--format=%(refname)", cwd=where)


def _local_state(h: Harness) -> tuple[str, str, str, str]:
    """Worktrees, local refs, origin refs and stashes: what a refusal must not touch."""
    return (
        h.git("worktree", "list"),
        _refs(h),
        _refs(h, h.origin),
        h.git("stash", "list"),
    )


def _no_run_files(h: Harness) -> None:
    grind = Path(h.repo) / ".clud" / "grind"
    assert not (grind / "run.json").exists()
    assert not (grind / "plan.json").exists()


def _gh_calls(h: Harness) -> list[list[str]]:
    return h.read_gh_state().get("calls", [])


def _tool_calls(result: RunResult) -> list[str]:
    return [
        h["tool_input"]["command"]
        for h in result.hooks
        if h.get("event") == "PreToolUse"
        and h.get("tool_name") == "Bash"
        and "is_meta_issue.py" in str((h.get("tool_input") or {}).get("command", ""))
    ]


def _attach(parent: str, child: str) -> str:
    return f"gh api -X POST repos/o/r/issues/{parent}/sub_issues -F sub_issue_id={child}"


def _verify(parent: str, children: tuple[str, ...]) -> str:
    """Both directions, as `/clud-issue`'s hierarchy mode verifies them."""
    return " && ".join(
        [
            f"gh api repos/o/r/issues/{parent}/sub_issues",
            *(f"gh api repos/o/r/issues/{c}/parent" for c in children),
        ]
    )


def _verified(h: Harness, parent: str, children: tuple[str, ...]) -> None:
    gets = [c for c in _gh_calls(h) if c[:1] == ["api"] and "-X" not in c]
    assert ["api", f"repos/o/r/issues/{parent}/sub_issues"] in gets, gets
    for c in children:
        assert ["api", f"repos/o/r/issues/{c}/parent"] in gets, (c, gets)


def _run(h: Harness, steps: list[dict[str, Any]], **kw: Any) -> RunResult:
    result = h.run("/grind", _main(steps), **kw)
    assert result.returncode == 0, result.stdout[-3000:]
    _no_notes(result)
    return result


def test_r1_prompt_with_three_deliverables_becomes_a_meta_with_three_children(
    harness: Harness,
) -> None:
    before = _issues(harness)
    children = ("1", "2", "3")
    steps = [
        _bash("gh issue create --title 'add --verbose' --body 'add a --verbose flag'"),
        _after(OK, _bash("gh issue create --title 'log steps' --body 'log each step'")),
        _after(OK, _bash("gh issue create --title 'document' --body 'document the flag'")),
        # The meta issue comes only after every child exists (R11).
        _after(OK, _bash("gh issue create --title 'meta: verbose' --body 'Tracks #1, #2, #3'")),
        _after(OK, _bash(" && ".join(_attach("4", c) for c in children))),
        _after(OK, _bash(_verify("4", children))),
        _after(_saw('"number": 4'), {"text": "META_READY #4"}),
    ]
    result = _run(harness, steps)
    assert _created(harness, before) == ["1", "2", "3", "4"]
    issues = _issues(harness)
    assert [s["number"] for s in issues["4"]["sub_issues"]] == [1, 2, 3]
    assert all(issues[c]["parent"] == 4 for c in children)
    posts = [c for c in _gh_calls(harness) if c[:1] == ["api"] and "POST" in c]
    assert len(posts) == 3, posts
    _verified(harness, "4", children)
    assert "META_READY #4" in result.stdout
    _no_workflow(result)


def test_r2_issue_list_gets_one_meta_and_nothing_else(harness: Harness) -> None:
    state = worlds._world({n: worlds._issue(f"task {n}", f"do {n}") for n in (7, 8, 9)})
    harness.write_gh_state(state)
    before = _issues(harness)
    children = ("7", "8", "9")
    steps = [
        _bash("gh issue create --title 'meta: 7 8 9' --body 'Tracks #7, #8, #9'"),
        _after(OK, _bash(" && ".join(_attach("10", c) for c in children))),
        _after(OK, _bash(_verify("10", children))),
        _after(_saw('"number": 10'), {"text": "META_READY #10"}),
    ]
    result = _run(harness, steps)
    assert _created(harness, before) == ["10"]
    issues = _issues(harness)
    assert [s["number"] for s in issues["10"]["sub_issues"]] == [7, 8, 9]
    _verified(harness, "10", children)
    _no_workflow(result)


def test_r3_native_sub_issue_meta_is_used_as_is(harness: Harness) -> None:
    harness.write_gh_state(worlds.meta_with_sub_issues())
    before = _issues(harness)
    steps = [
        _bash(f"{TOOL} 1 --repo o/r"),
        _after(_saw('"meta": true'), {"text": "IS_META"}),
    ]
    result = _run(harness, steps)
    assert "IS_META" in result.stdout
    assert _tool_calls(result) == [f"{TOOL} 1 --repo o/r"]
    assert _questions(result) == []
    assert _issues(harness) == before


def test_r4_task_list_meta_is_used_as_is(harness: Harness) -> None:
    harness.write_gh_state(worlds.task_list_meta())
    before = _issues(harness)
    steps = [
        _bash(f"{TOOL} 1 --repo o/r"),
        # No native sub-issues: the tool answers from the body's task list.
        _after(_saw('"task_list_refs": [2, 3, 4]'), {"text": "IS_META"}),
    ]
    result = _run(harness, steps)
    assert "IS_META" in result.stdout
    assert _questions(result) == []
    assert _issues(harness) == before


def test_r5_multi_part_issue_converted_on_consent(harness: Harness) -> None:
    harness.write_gh_state(worlds.multi_part())
    before = _issues(harness)
    create = "gh issue create --title '{t}' --body '> {t} (Split from #1)'"
    children = ("2", "3", "4")
    split = "Split into meta #5; closes when #5 closes."
    steps = [
        _bash(f"{TOOL} 1 --repo o/r"),
        _after(_saw('"meta": false'), _ask()),
        _after(_saw(CONVERT), _bash(create.format(t=CHILDREN[0]))),
        _after(OK, _bash(create.format(t=CHILDREN[1]))),
        _after(OK, _bash(create.format(t=CHILDREN[2]))),
        _after(
            OK,
            _bash("gh issue create --title 'meta: verbose mode' --body 'Tracks #1: #2, #3, #4'"),
        ),
        _after(OK, _bash(" && ".join(_attach("5", c) for c in children))),
        _after(OK, _bash(_verify("5", children))),
        _after(_saw('"number": 5'), _bash(f"gh issue comment 1 --body '{split}'")),
        _after(OK, {"text": "CONVERTED #5"}),
    ]
    result = _run(harness, steps, answers={"*": CONVERT})
    asked = _questions(result)
    assert len(asked) == 1, asked
    # The conversion question lists every proposed child and both options.
    question = asked[0]["tool_input"]["questions"][0]
    assert all(t in question["question"] for t in CHILDREN), question
    assert [o["label"] for o in question["options"]] == [CONVERT, ABORT]
    assert _created(harness, before) == ["2", "3", "4", "5"]
    issues = _issues(harness)
    for c, part in zip(children, CHILDREN, strict=True):
        assert issues[c]["body"].startswith(f"> {part}"), issues[c]["body"]
        assert "Split from #1" in issues[c]["body"]
    assert issues["5"]["body"].startswith("Tracks #1")
    assert [s["number"] for s in issues["5"]["sub_issues"]] == [2, 3, 4]
    _verified(harness, "5", children)
    assert [c["body"] for c in issues["1"]["comments"]] == [split]
    assert issues["1"]["state"] == "open"
    assert "CONVERTED #5" in result.stdout
    _no_workflow(result)


def test_r6_multi_part_issue_left_alone_on_refusal(harness: Harness) -> None:
    harness.write_gh_state(worlds.multi_part())
    before = _issues(harness)
    local = _local_state(harness)
    steps = [
        _bash(f"{TOOL} 1 --repo o/r"),
        _after(_saw('"meta": false'), _ask()),
        _after(_saw(ABORT), {"text": REFUSAL}),
    ]
    result = _run(harness, steps, answers={"*": ABORT})
    assert "run `/do 1`" in result.stdout
    assert len(_questions(result)) == 1
    # Nothing created: no issues, comments or labels on GitHub, and no run
    # files, worktrees, branches or stashes locally.
    assert _issues(harness) == before
    assert _local_state(harness) == local
    _no_run_files(harness)
    assert not [c for c in _gh_calls(harness) if c[:2] in (["issue", "create"], ["issue", "edit"])]
    _no_workflow(result)


def test_r7_single_change_issue_is_sent_to_do_without_asking(harness: Harness) -> None:
    harness.write_gh_state(worlds.single_change())
    before = _issues(harness)
    local = _local_state(harness)
    steps = [
        _bash(f"{TOOL} 1 --repo o/r"),
        _after(_saw('"meta": false'), {"text": REFUSAL}),
    ]
    result = _run(harness, steps)
    assert "run `/do 1`" in result.stdout
    assert _questions(result) == []
    assert _issues(harness) == before
    assert _local_state(harness) == local
    _no_run_files(harness)
    _no_workflow(result)


def test_r8_meta_with_every_sub_issue_closed_has_nothing_to_do(harness: Harness) -> None:
    state = worlds.meta_with_sub_issues()
    for n in ("2", "3", "4"):
        state["issues"][n]["state"] = "closed"
    for s in state["issues"]["1"]["sub_issues"]:
        s["state"] = "closed"
    harness.write_gh_state(state)
    before = _issues(harness)
    local = _local_state(harness)
    steps = [
        _bash(f"{TOOL} 1 --repo o/r"),
        _after(
            _saw('"state": "closed"'),
            {"text": "Nothing to do: every sub-issue of #1 is closed."},
        ),
    ]
    result = _run(harness, steps)
    assert "Nothing to do" in result.stdout
    assert _issues(harness) == before
    assert _local_state(harness) == local
    _no_run_files(harness)
    _no_workflow(result)


@pytest.mark.parametrize(
    "fault",
    [
        {"faults": {"api GET": {"code": 1, "stderr": "gh: Server Error (HTTP 502)"}}},
        {"fail": True},
    ],
    ids=["sub-issues-502", "gh-fail"],
)
def test_r9_gh_failure_surfaces_and_changes_nothing(
    harness: Harness, fault: dict[str, Any]
) -> None:
    state = worlds.meta_with_sub_issues()
    state.update(json.loads(json.dumps(fault)))
    harness.write_gh_state(state)
    before = _issues(harness)
    steps = [
        _bash(f"{TOOL} 1 --repo o/r; echo EXIT=$?"),
        # Exit 2 and nothing on stdout: a failure never reads as "not meta".
        _after(_saw("EXIT=2"), {"text": "is_meta_issue.py failed (exit 2); not guessing."}),
    ]
    result = _run(harness, steps)
    assert "is_meta_issue.py failed" in result.stdout
    assert _questions(result) == []
    assert _issues(harness) == before
    _no_run_files(harness)
    _no_workflow(result)


@pytest.mark.parametrize("target", ["1 --repo o/r", "1", "https://github.com/o/r/issues/1"])
def test_r10_url_and_bare_number_route_the_same(harness: Harness, target: str) -> None:
    harness.write_gh_state(worlds.meta_with_sub_issues())
    before = _issues(harness)
    steps = [
        _bash(f"{TOOL} {target}"),
        _after(_saw('"meta": true'), {"text": "IS_META"}),
    ]
    result = _run(harness, steps)
    assert "IS_META" in result.stdout
    assert _questions(result) == []
    assert _issues(harness) == before
    sub_calls = [c for c in _gh_calls(harness) if c[:1] == ["api"]]
    assert sub_calls[0][1] == "repos/o/r/issues/1/sub_issues"


def test_r11_second_child_create_fails_and_no_workflow_starts(harness: Harness) -> None:
    state = worlds.multi_part()
    # The first child is created; every later `gh issue create` fails.
    state["faults"] = {
        "issue create": {"code": 1, "stderr": "gh: Server Error (HTTP 502)", "after": 1}
    }
    harness.write_gh_state(state)
    before = _issues(harness)
    create = "gh issue create --title '{t}' --body '> {t} (Split from #1)'"
    steps = [
        _bash(f"{TOOL} 1 --repo o/r"),
        _after(_saw('"meta": false'), _ask()),
        _after(_saw(CONVERT), _bash(create.format(t=CHILDREN[0]))),
        _after(OK, _bash(create.format(t=CHILDREN[1]) + "; echo EXIT=$?")),
        _after(
            _saw("EXIT=1"),
            {"text": "Conversion failed at child 2 (HTTP 502). Partial state: created #2."},
        ),
    ]
    result = _run(harness, steps, answers={"*": CONVERT})
    assert "Partial state: created #2" in result.stdout
    # No orphaned meta issue: children come first, so only child #2 exists,
    # nothing is attached, and the original gets no split comment.
    assert _created(harness, before) == ["2"]
    assert _issues(harness)["1"]["comments"] == []
    assert not [c for c in _gh_calls(harness) if c[:1] == ["api"] and "POST" in c]
    _no_run_files(harness)
    _no_workflow(result)
