"""`grind-run`'s read-only planning pass on the real Claude Code (#1406).

With `planOnly: true` the workflow runs one `grind-planner` over the meta
issue's children, prints the bug/feature classification, and picks a path:
`simple` (keep the meta issue as is) under the complexity threshold,
`regroup` above it. Covers #1392 cases U2, T4 and H1-H5 (H7 is in
`test_grind_tracks.py`). The planning phase is marked by
`<repo>/.clud/grind/run.json` with `{"phase": "plan"}`, which the clud hook
reads to hold the planner read-only; the prompt's PLAN-ONLY text is for the
planner only.

The Workflow result reaches the main session as a later task notification
carrying the return value, never the `log()` lines, so the classification
`lines` and the `keeping #T as is:` `message` are asserted from the return.

Script note: a step's `expect` checks the tool results of the *previous*
step, so an expectation about a command sits on the step after it.
"""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any

import pytest

from tests.harness.harness import Harness, RunResult
from tests.harness.worlds import _issue, _world

MARK = {"planner": "You are the /grind planner"}
META = "100"


def _structured(value: dict[str, Any]) -> dict[str, Any]:
    return {"tool_use": {"name": "StructuredOutput", "input": value}}


def _bash(command: str) -> dict[str, Any]:
    return {"tool_use": {"name": "Bash", "input": {"command": command, "description": "grind"}}}


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


def _plan_phase(h: Harness, mode: str | None = None) -> None:
    run = Path(h.repo) / ".clud" / "grind" / "run.json"
    run.parent.mkdir(parents=True, exist_ok=True)
    facts: dict[str, Any] = {"phase": "plan"}
    if mode is not None:
        facts["mode"] = mode
    run.write_text(json.dumps(facts), encoding="utf-8")


def _script(
    h: Harness, children: list[int], planner_steps: list[dict[str, Any]]
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
            {"tool_use": {"name": "Workflow", "input": {"name": "grind-run", "args": args}}},
            {"text": "PLANNED"},
        ],
    }
    return {
        "default_text": "OK",
        "roles": [_role("planner", "planner", "PLAN-ONLY", planner_steps), main],
    }


def _classify(
    tracks: dict[int, tuple[str, str | None]],
    groups: dict[str, list[int]],
    *,
    independent: bool = True,
    confident: bool = True,
) -> dict[str, Any]:
    children = []
    for n, (track, group) in tracks.items():
        child: dict[str, Any] = {"id": str(n), "track": track, "depends_on_bugs": []}
        if group is not None:
            child["group"] = group
        children.append(child)
    return {
        "children": children,
        "groups": [
            {"name": g, "independent": independent, "children": [str(n) for n in ns]}
            for g, ns in groups.items()
        ],
        "order": [str(n) for n in tracks],
        "confident": confident,
    }


def _told(result: RunResult) -> str:
    """Everything the main session saw, plus stdout, unescaped."""
    seen = [r["messages"] for r in result.requests if r["role"] == "main"]
    return json.dumps(seen, ensure_ascii=False) + result.stdout


def _no_notes(result: RunResult) -> None:
    """No scripted step failed its expectation or asked for a missing tool."""
    notes = [(r["role"], r["note"]) for r in result.requests if r.get("note")]
    assert not notes, notes


def _path(result: RunResult) -> str | None:
    match = re.search(r'path\\*"\s*:\s*\\*"(simple|regroup)', _told(result))
    return match.group(1) if match else None


def _run(h: Harness, children: list[int], plan: dict[str, Any]) -> tuple[RunResult, dict]:
    before = _seed(h, children)
    _plan_phase(h)
    result = h.run("start grind", _script(h, children, [_structured(plan)]), timeout=300)
    assert result.returncode == 0, result.stdout[-3000:]
    return result, before


def test_plan_only_planner_is_read_only(harness: Harness) -> None:
    """U2: no worktree, push or file write gets through in plan-only mode.

    Write/Edit are not offered to `grind-planner` at all (its `tools:` list),
    so the planner cannot even attempt them; a scripted Write step would only
    be answered with text by the mock and end the planner. The hook's
    plan-only mode covers the shell: `git worktree add`, which parallel mode
    would otherwise allow, and push.
    """
    repo = str(harness.repo)
    plan = _classify({101: ("bug", None)}, {})
    worktree = f"{repo}-wt-1"
    add = f"git -C {repo} worktree add {worktree} -b grind/x origin/main"
    push = f"git -C {repo} push origin main"
    steps = [
        _bash(add),
        _after({"is_error": True, "content_contains": "plan-only"}, _bash(push)),
        _after({"is_error": True, "content_contains": "git push"}, _structured(plan)),
    ]
    _seed(harness, [101])
    # Parallel mode: only plan-only mode refuses the worktree.
    _plan_phase(harness, mode="parallel")
    result = harness.run("start grind", _script(harness, [101], steps), timeout=300)
    assert result.returncode == 0, result.stdout[-3000:]
    tools = set(result.first_request("planner")["tools"])
    assert "Write" not in tools, tools
    assert "Edit" not in tools, tools
    bash = [
        h["tool_input"]["command"]
        for h in result.hooks
        if h.get("agent_type") == "grind-planner"
        and h.get("event") == "PreToolUse"
        and h.get("tool_name") == "Bash"
    ]
    assert bash == [add, push], bash
    assert not [
        h
        for h in result.hooks
        if h.get("agent_type") == "grind-planner" and h.get("tool_name") in ("Write", "Edit")
    ]
    _no_notes(result)
    assert _path(result) == "simple", _told(result)[-3000:]
    assert not Path(worktree).exists()
    assert "grind/x" not in harness.git("branch", "--list", "grind/x")
    heads = harness.git(
        "for-each-ref", "--format=%(refname:short)", "refs/heads", cwd=harness.origin
    )
    assert heads.split() == ["main"], heads
    planner = json.dumps([r["messages"] for r in result.requests if r["role"] == "planner"])
    assert planner.count('"is_error": true') >= 2, planner[-3000:]


def test_classification_is_printed_without_asking(harness: Harness) -> None:
    plan = _classify(
        {101: ("bug", None), 102: ("feature", "auth"), 103: ("feature", "auth")},
        {"auth": [102, 103]},
    )
    result, _ = _run(harness, [101, 102, 103], plan)
    told = _told(result)
    assert "#101 bug → main" in told, told[-3000:]
    assert "#102 feature → grind/meta-100-auth" in told
    assert "#103 feature → grind/meta-100-auth" in told
    for n in (101, 102, 103):
        lines = {ln.strip() for ln in re.split(r"\\n|\n", told) if re.search(rf"#{n} \w+ →", ln)}
        assert len(lines) == 1, (n, lines)
    # No AskUserQuestion anywhere: `questions_before` of an absent role
    # returns every question record in the run.
    assert result.questions_before("no-such-role") == []


def _groups(*spec: tuple[str, int]) -> tuple[dict, dict]:
    """(`tracks`, `groups`) from (name, size) pairs; name 'bug' means bugs."""
    tracks: dict[int, tuple[str, str | None]] = {}
    groups: dict[str, list[int]] = {}
    n = 101
    for name, size in spec:
        for _ in range(size):
            if name == "bug":
                tracks[n] = ("bug", None)
            elif name == "ungrouped":
                tracks[n] = ("feature", None)
            else:
                tracks[n] = ("feature", name)
                groups.setdefault(name, []).append(n)
            n += 1
    return tracks, groups


SIMPLE = {
    "H1-one-group-of-5": (("a", 5),),
    "H2-two-groups-of-3-plus-bug": (("a", 3), ("b", 3), ("bug", 1)),
    "H3-a-group-of-2": (("a", 2), ("b", 4), ("bug", 2)),
    "H5-an-ungrouped-feature": (("a", 4), ("b", 3), ("ungrouped", 1)),
}


@pytest.mark.parametrize("spec", list(SIMPLE.values()), ids=list(SIMPLE))
def test_below_threshold_keeps_the_meta_issue(harness: Harness, spec: tuple) -> None:
    tracks, groups = _groups(*spec)
    result, before = _run(harness, list(tracks), _classify(tracks, groups))
    assert _path(result) == "simple", _told(result)[-3000:]
    assert f"keeping #{META} as is:" in _told(result)
    assert harness.read_gh_state() == before


REGROUP = {
    "H4-two-groups-of-3-and-2-bugs": (("a", 3), ("b", 3), ("bug", 2)),
    "two-groups-of-4": (("a", 4), ("b", 4)),
}


@pytest.mark.parametrize("spec", list(REGROUP.values()), ids=list(REGROUP))
def test_above_threshold_chooses_regroup(harness: Harness, spec: tuple) -> None:
    tracks, groups = _groups(*spec)
    result, before = _run(harness, list(tracks), _classify(tracks, groups))
    assert _path(result) == "regroup", _told(result)[-3000:]
    assert f"keeping #{META} as is" not in _told(result)
    # The plan-only pass only reports; the regroup itself waits for the answer.
    assert harness.read_gh_state() == before


@pytest.mark.parametrize(
    "unsure", [{"confident": False}, {"independent": False}], ids=["not-confident", "dependent"]
)
def test_unsure_classification_keeps_the_meta_issue(harness: Harness, unsure: dict) -> None:
    """H5's other faces: an unconfident plan, or groups that are not independent."""
    tracks, groups = _groups(("a", 4), ("b", 4))
    result, before = _run(harness, list(tracks), _classify(tracks, groups, **unsure))
    assert _path(result) == "simple", _told(result)[-3000:]
    assert f"keeping #{META} as is:" in _told(result)
    assert harness.read_gh_state() == before


def test_a_child_the_planner_left_out_is_unplaced(harness: Harness) -> None:
    """Every child is printed even if the planner omits it, and it keeps the
    meta issue as is, though the rest would clear the threshold."""
    tracks, groups = _groups(("a", 4), ("b", 4))
    plan = _classify(tracks, groups)
    children = [*tracks, 109]
    result, before = _run(harness, children, plan)
    told = _told(result)
    assert _path(result) == "simple", told[-3000:]
    assert "#109 unclassified" in told, told[-3000:]
    assert "could not be placed" in told
    assert harness.read_gh_state() == before
