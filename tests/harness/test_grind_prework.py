"""`grind-run`'s prework role on the real Claude Code (#1408).

When the router passes `plan` (the assembled `grind-plan/v1` plan) and
`meta`, the workflow builds the public plan comment body (or split parts)
and a `grind-prework` agent posts it on the meta issue before any worker
runs. Covers #1392 section 3, cases P1-P9.

The scripted model cannot copy text out of its prompt, so two things are
checked separately: the bodies the *workflow* built (read from the prework
agent's first request) and the comment the prework agent actually posted
(read from the fake GitHub). The run facts the hook reads (`mode`, `meta`)
live in `<repo>/.clud/grind/run.json`, written here as the router would.

Script note: a step's `expect` checks the tool results of the *previous*
step, so an expectation about a command sits on the step after it.
"""

from __future__ import annotations

import json
import re
import shlex
from pathlib import Path
from typing import Any

from tests.harness.harness import Harness, RunResult
from tests.harness.worlds import _issue, _world, with_user_sub_meta

MARK = {
    "prework": "You are the /grind prework role",
    "planner": "You are the /grind planner",
    "worker": "You are a /grind worker",
    "reviewer": "You are the /grind reviewer",
    "integrator": "You are the /grind integrator",
    "lander": "You are the /grind lander",
}
META = "100"
RUN_ID = "r-1408"
STASH = "grind-preflight-stash-1408"
START_BRANCH = "wip/start-1408"
PLAN_LIMIT = 65536
MARKER = "<!-- grind:v1 plan run="
# fake_gh numbers comments from `next_id` (1000 in every world here).
FIRST_COMMENT_ID = 1000


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


def _comment_url(meta: str, cid: int = FIRST_COMMENT_ID) -> str:
    return f"https://github.com/o/r/issues/{meta}#issuecomment-{cid}"


def _seed(h: Harness, children: list[int]) -> dict[str, Any]:
    """Meta #100 with `children` as native sub-issues; returns the state."""
    issues = {int(META): _issue("meta: backlog", "Tracked as sub-issues.")}
    for n in children:
        issues[n] = _issue(f"child {n}", f"do {n}", parent=int(META))
    h.write_gh_state(_world(issues))
    return h.read_gh_state()


def _run_facts(h: Harness, meta: str) -> None:
    run = Path(h.repo) / ".clud" / "grind" / "run.json"
    run.parent.mkdir(parents=True, exist_ok=True)
    run.write_text(json.dumps({"mode": "sequential", "meta": meta}), encoding="utf-8")


def _plan(h: Harness, meta: str, children: list[dict[str, Any]]) -> dict[str, Any]:
    """The router's assembled plan, local-only fields included."""
    return {
        "schema": "grind-plan/v1",
        "run_id": RUN_ID,
        "meta": meta,
        "original": f"https://github.com/o/r/issues/{meta}",
        "repo": "o/r",
        "main": "main",
        "mode": "sequential",
        "preflight": {
            "stash": STASH,
            "start_branch": START_BRANCH,
            "repo_path": str(h.repo),
        },
        "structure": "simple",
        "stages": [{"name": "bugs", "checkout": str(h.repo), "children": children}],
        "deferred_groups": [],
        "feature_merge": {"strategy": "one-pr-per-group", "base": "main"},
        "problem_reporting": {"where": "meta-comment", "status_comment": True},
        "models": {},
        "ci": False,
        "scripts": {},
        "rules": ["bugs land on main first"],
    }


def _save_local_plan(h: Harness, plan: dict[str, Any]) -> Path:
    """`.clud/grind/plan.json`: the full plan, as the router keeps it locally."""
    path = Path(h.repo) / ".clud" / "grind" / "plan.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(plan, indent=1), encoding="utf-8")
    return path


def _public_body(plan: dict[str, Any]) -> str:
    """What the prework agent posts: marker plus fenced public plan."""
    pub = json.loads(json.dumps(plan))
    pub["preflight"] = "handled"
    for stage in pub["stages"]:
        stage.pop("checkout", None)
    return f"{MARKER}{plan['run_id']} -->\n```json\n{json.dumps(pub, indent=1)}\n```"


def _post(meta: str, body: str) -> dict[str, Any]:
    return _bash(f"gh issue comment {meta} --repo o/r --body {shlex.quote(body)}")


def _prework(meta: str, steps: list[dict[str, Any]] | None = None, *, body: str = "") -> dict:
    url = _comment_url(meta)
    default = [
        _post(meta, body),
        _after(
            {"is_error": False},
            _structured({"posted": True, "plan_url": url, "part_urls": [url]}),
        ),
    ]
    return _role("prework", "prework", "/grind-prework", steps or default)


def _goal_roles(h: Harness, goal: str) -> list[dict[str, Any]]:
    """One goal's planner, worker, reviewer, integrator and lander."""
    branch = f"grind/{goal}"
    repo = str(h.repo)
    at = f"Goal {goal}:"
    ok = {"is_error": False}
    plan = {
        "checkout": repo,
        "branch": branch,
        "depends_on": [],
        "verify": "true",
        "tasks": [{"id": "t1", "files": [f"{goal}.txt"], "instructions": f"write {goal}.txt"}],
    }
    write = {"file_path": f"{repo}/{goal}.txt", "content": f"{goal}\n"}
    push = (
        f"git -C {repo} switch -q main && git -C {repo} switch -q -c {branch} && "
        f"git -C {repo} add {goal}.txt && git -C {repo} commit -q -m {goal} && "
        f"git -C {repo} push -q -u origin {branch} && gh pr create --title {goal} --head {branch}"
    )
    url = f"https://github.com/o/r/pull/{branch}"
    return [
        _role(f"planner:{goal}", "planner", at, [_structured(plan)]),
        _role(
            f"worker:{goal}",
            "worker",
            at,
            [
                {"tool_use": {"name": "Write", "input": write}},
                _after(ok, _structured({"files_touched": [f"{goal}.txt"], "summary": "wrote"})),
            ],
        ),
        _role(
            f"reviewer:{goal}", "reviewer", at, [_structured({"approved": True, "summary": "ok"})]
        ),
        _role(
            f"integrator:{goal}",
            "integrator",
            at,
            [_bash(push), _after(ok, _structured({"pushed": True, "pr_url": url, "summary": "p"}))],
        ),
        _role(
            f"lander:{goal}",
            "lander",
            [at, "Fix rounds used: 0 of"],
            [
                _bash(f"gh pr merge {branch} --admin --squash"),
                _after(ok, _structured({"status": "merged", "summary": "green"})),
            ],
        ),
    ]


def _script(
    h: Harness,
    meta: str,
    goals: list[str],
    plan: dict[str, Any],
    roles: list[dict[str, Any]],
) -> dict[str, Any]:
    args = {
        "repo": str(h.repo),
        "main": "main",
        "mode": "sequential",
        "meta": meta,
        "plan": plan,
        "goals": [{"id": g, "title": f"child {g}", "brief": f"do {g}"} for g in goals],
    }
    main = {
        "name": "main",
        "steps": [
            {"tool_use": {"name": "Workflow", "input": {"name": "grind-run", "args": args}}},
            {"text": "GRIND_DONE"},
        ],
    }
    return {"default_text": "OK", "roles": [*roles, main]}


def _text(value: Any) -> str:
    """Every string inside `value`, joined: a request's raw prompt text."""
    if isinstance(value, str):
        return value
    if isinstance(value, dict):
        return "\n".join(_text(v) for v in value.values())
    if isinstance(value, list):
        return "\n".join(_text(v) for v in value)
    return ""


def _built_bodies(result: RunResult) -> list[str]:
    """The comment bodies the workflow handed the prework agent."""
    prompt = _text(result.first_request("prework").get("messages"))
    parts = re.split(r"--- body \d+ of \d+ ---\n", prompt)[1:]
    assert parts, prompt[-3000:]
    return [p.rstrip("\n") for p in parts]


def _json_block(body: str) -> dict[str, Any]:
    match = re.search(r"```json\n(.*?)\n```", body, re.DOTALL)
    assert match, body[:2000]
    return json.loads(match.group(1))


def _plan_comments(h: Harness, meta: str) -> list[dict[str, Any]]:
    comments = h.read_gh_state()["issues"][meta].get("comments", [])
    return [c for c in comments if MARKER in c.get("body", "")]


def _no_notes(result: RunResult) -> None:
    notes = [(r["role"], r["note"]) for r in result.requests if r.get("note")]
    assert not notes, notes


def _told(result: RunResult) -> str:
    seen = [r["messages"] for r in result.requests if r["role"] == "main"]
    return json.dumps(seen, ensure_ascii=False) + result.stdout


def _full_run(h: Harness) -> tuple[RunResult, dict[str, Any], Path]:
    _seed(h, [101])
    _run_facts(h, META)
    plan = _plan(h, META, [{"id": "101", "title": "child 101", "track": "bug"}])
    local = _save_local_plan(h, plan)
    roles = [_prework(META, body=_public_body(plan)), *_goal_roles(h, "101")]
    result = h.run("start grind", _script(h, META, ["101"], plan, roles), timeout=300)
    assert result.returncode == 0, result.stdout[-3000:]
    _no_notes(result)
    return result, plan, local


def test_plan_comment_is_posted_once_with_the_public_plan(harness: Harness) -> None:
    result, plan, local = _full_run(harness)
    # P1: exactly one plan comment on the meta issue, with a grind-plan/v1 block.
    posted = _plan_comments(harness, META)
    assert len(posted) == 1, posted
    assert _json_block(posted[0]["body"])["schema"] == "grind-plan/v1"
    bodies = _built_bodies(result)
    assert len(bodies) == 1, bodies
    assert bodies[0].startswith(f"{MARKER}{RUN_ID} -->"), bodies[0][:300]
    built = _json_block(bodies[0])
    assert built["schema"] == "grind-plan/v1"
    # P2: the recorded plan is the plan the router passed.
    assert built["feature_merge"] == plan["feature_merge"]
    assert built["problem_reporting"] == plan["problem_reporting"]
    assert [s["children"] for s in built["stages"]] == [s["children"] for s in plan["stages"]]
    # P3: local-only state stays local.
    assert built["preflight"] == "handled"
    for secret in (STASH, START_BRANCH, str(harness.repo)):
        assert secret not in bodies[0], secret
        assert secret not in posted[0]["body"], secret
    saved = json.loads(local.read_text(encoding="utf-8"))
    assert saved["preflight"]["stash"] == STASH
    assert saved["preflight"]["start_branch"] == START_BRANCH
    assert saved["preflight"]["repo_path"] == str(harness.repo)


def test_every_later_role_is_told_the_plan_url(harness: Harness) -> None:
    result, _, _ = _full_run(harness)
    url = _comment_url(META)
    # P4: each later role's first request carries the plan comment URL.
    for role in ("planner:101", "worker:101", "reviewer:101", "integrator:101", "lander:101"):
        assert url in _text(result.first_request(role).get("messages")), role
    # Prework ran before any of them.
    first = {r["role"]: r["n"] for r in reversed(result.requests)}
    assert first["prework"] < first["planner:101"]


def test_plan_comment_is_never_edited(harness: Harness) -> None:
    _full_run(harness)
    # P5: nothing PATCHes the plan comment after prework.
    calls = harness.read_gh_state().get("calls", [])
    edits = [
        c
        for c in calls
        if c[:1] == ["api"]
        and any(f"comments/{FIRST_COMMENT_ID}" in a for a in c)
        and any(a.upper() == "PATCH" for a in c)
    ]
    assert not edits, edits
    assert "edit-last" not in json.dumps(calls)


def test_oversized_plan_is_split_into_parts_under_the_limit(harness: Harness) -> None:
    many = list(range(101, 501))
    _seed(harness, many)
    _run_facts(harness, META)
    children = [{"id": str(n), "title": f"child {n} " + "x" * 300, "track": "bug"} for n in many]
    plan = _plan(harness, META, children)
    # The prework agent stops the run once it has seen the bodies.
    steps = [_structured({"posted": False, "error": "harness stops here"})]
    roles = [_prework(META, steps)]
    result = harness.run("start grind", _script(harness, META, ["101"], plan, roles), timeout=300)
    assert result.returncode == 0, result.stdout[-3000:]
    # P6: split into part=k/N comments, each under GitHub's body limit.
    bodies = _built_bodies(result)
    n = len(bodies)
    assert n >= 2, [len(b) for b in bodies]
    for k, body in enumerate(bodies, start=1):
        assert body.startswith(f"{MARKER}{RUN_ID} part={k}/{n} -->"), body[:200]
        assert len(body) < PLAN_LIMIT, (k, len(body))
    assert _json_block(bodies[0])["schema"] == "grind-plan/v1"
    ids = [
        child["id"]
        for body in bodies[1:]
        for stage in _json_block(body)["stages"]
        for child in stage["children"]
    ]
    assert ids == [str(n) for n in many]


def test_prework_shell_is_capped_to_commenting_on_the_meta_issue(harness: Harness) -> None:
    repo = str(harness.repo)
    _seed(harness, [101])
    _run_facts(harness, META)
    plan = _plan(harness, META, [{"id": "101", "title": "child 101", "track": "bug"}])
    denied = {"is_error": True, "content_contains": "/grind role caps"}
    ok = {"is_error": False}
    worktree = f"{repo}-wt-prework"
    url = _comment_url(META)
    steps = [
        _bash(f"git -C {repo} worktree add {worktree} -b grind/x origin/main"),
        _after(denied, _bash("gh issue create --title x --body y")),
        _after(denied, _bash("gh issue comment 101 --body hijack")),
        _after(denied, _post(META, _public_body(plan))),
        _after(ok, _structured({"posted": True, "plan_url": url, "part_urls": [url]})),
    ]
    roles = [_prework(META, steps), *_goal_roles(harness, "101")]
    result = harness.run("start grind", _script(harness, META, ["101"], plan, roles), timeout=300)
    assert result.returncode == 0, result.stdout[-3000:]
    # P7: no Edit/Write at all, and the hook's decisions show in the results.
    tools = set(result.first_request("prework")["tools"])
    assert "Edit" not in tools and "Write" not in tools, tools
    _no_notes(result)
    bash = [
        h["tool_input"]["command"]
        for h in result.hooks
        if h.get("agent_type") == "grind-prework"
        and h.get("event") == "PreToolUse"
        and h.get("tool_name") == "Bash"
    ]
    assert len(bash) == 4, bash
    assert not Path(worktree).exists()
    state = harness.read_gh_state()
    assert set(state["issues"]) == {META, "101"}
    assert not state["issues"]["101"].get("comments")
    assert len(_plan_comments(harness, META)) == 1


def test_failed_plan_comment_stops_before_any_worker(harness: Harness) -> None:
    _seed(harness, [101])
    state = harness.read_gh_state()
    state["faults"] = {"issue comment": {"code": 1, "stderr": "gh: HTTP 502"}}
    harness.write_gh_state(state)
    _run_facts(harness, META)
    plan = _plan(harness, META, [{"id": "101", "title": "child 101", "track": "bug"}])
    steps = [
        _post(META, _public_body(plan)),
        _after(
            {"is_error": True},
            _structured({"posted": False, "part_urls": [], "error": "gh: HTTP 502"}),
        ),
    ]
    roles = [_prework(META, steps), *_goal_roles(harness, "101")]
    result = harness.run("start grind", _script(harness, META, ["101"], plan, roles), timeout=300)
    assert result.returncode == 0, result.stdout[-3000:]
    # P8: no worker ran, and the workflow says it stopped at prework.
    ran = {r["role"] for r in result.requests}
    assert "worker:101" not in ran, ran
    assert not any(h.get("agent_type") == "grind-worker" for h in result.hooks)
    told = _told(result)
    assert "prework" in told, told[-3000:]
    assert "stopping before any worker" in told, told[-3000:]
    assert not _plan_comments(harness, META)


def test_meta_of_metas_gets_the_plan_on_the_top_issue(harness: Harness) -> None:
    # P9: in a meta-of-metas world the plan lands on the top meta issue (#1).
    harness.write_gh_state(with_user_sub_meta())
    top = "1"
    _run_facts(harness, top)
    plan = _plan(
        harness,
        top,
        [{"id": str(n), "title": f"leaf {n}", "track": "bug"} for n in (2, 4, 5)],
    )
    url = _comment_url(top)
    steps = [
        _post(top, _public_body(plan)),
        _after(
            {"is_error": False},
            _structured({"posted": True, "plan_url": url, "part_urls": [url]}),
        ),
    ]
    # Stop after prework: the planner dies, so no goal proceeds.
    planner = _role("planner:2", "planner", "Goal 2:", [{"error": {"status": 400}}])
    roles = [_prework(top, steps), planner]
    result = harness.run("start grind", _script(harness, top, ["2"], plan, roles), timeout=300)
    assert result.returncode == 0, result.stdout[-3000:]
    assert f"Meta issue: #{top}." in _text(result.first_request("prework").get("messages"))
    assert len(_plan_comments(harness, top)) == 1
    assert not _plan_comments(harness, "3")
