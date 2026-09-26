"""The three `/grind` user scenarios of #1392, end to end on the real Claude Code (#1413).

Each test drives one whole run: routing, the single question round, prework,
the bug stage, the feature stage (or its deferral) and Finish. The main
session is scripted, so the router's `gh`/`git` commands are fixed in the
script, and every grind role is scripted too. What the tests guard is the
contract between them: which roles run and in what order, what the hook lets
through, and the final fake GitHub and git state.

1. **Bugs only** (`meta_with_sub_issues(4)`): four unrelated bugs land on
   `main` with `Closes #N`; one worker's problem becomes a `grind:followup`
   issue; the meta issue is never closed by hand and no feature branch exists.
2. **A prompt building a feature, auto merge**: routing turns the prompt into
   meta #6 (the `mixed()` backlog holds #1-#5) with bugs #7, #8 and feature
   parts #9, #10, #11 (#11 depends on #8). The checkout is dirty and is
   stashed up front. Bugs land on `main` first; the feature goals land on the
   feature branch; the draft feature PR is readied and merge-committed, and
   it alone closes #6 and the feature parts. `main` moving is merged in.
3. **A big messy epic, decided later** (`regroupable()` plus two user-made
   sub-metas and four bugs): the regroup rewrites the user's sub-metas in
   place, the user answers "Decide later" to the feature pick, so only the
   bug stage runs; both feature groups come back `deferred` and stay open.

Scenario 2 differs from #1392's prose in one scripted detail: the feature
branch and draft PR exist before the workflow starts (the router sets them up
from its own pre-steps), because the workflow is one tool call and the main
session cannot act between its stages.

Script note: a step's `expect` checks the tool results of the *previous*
step, so an expectation about a command sits on the step after it.
"""

from __future__ import annotations

import json
import shlex
from pathlib import Path
from typing import Any

from tests.harness.harness import Harness, RunResult
from tests.harness.test_grind_meta_of_metas import PREVIOUS, V1, _regroup
from tests.harness.worlds import _issue, meta_with_sub_issues, mixed, regroupable

MARK = {
    "prework": "You are the /grind prework role",
    "planner": "You are the /grind planner",
    "worker": "You are a /grind worker",
    "reviewer": "You are the /grind reviewer",
    "integrator": "You are the /grind integrator",
    "lander": "You are the /grind lander",
}
PLAN_MARKER = "<!-- grind:v1 plan run="
FOLLOWUP = "grind:followup"
OK = {"is_error": False}


def _structured(value: dict[str, Any]) -> dict[str, Any]:
    return {"tool_use": {"name": "StructuredOutput", "input": value}}


def _bash(command: str) -> dict[str, Any]:
    return {"tool_use": {"name": "Bash", "input": {"command": command, "description": "grind"}}}


def _write(path: Path, content: str) -> dict[str, Any]:
    return {"tool_use": {"name": "Write", "input": {"file_path": str(path), "content": content}}}


def _after(expect: dict[str, Any], step: dict[str, Any]) -> dict[str, Any]:
    """`step`, first checking the previous step's tool results."""
    return {**step, "expect": expect}


def _saw(text: str) -> dict[str, Any]:
    return {"is_error": False, "content_contains": text}


def _chain(steps: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Each step after the first expects the previous one to have succeeded,
    unless it already carries its own expectation."""
    return [s if i == 0 or "expect" in s else _after(OK, s) for i, s in enumerate(steps)]


def _role(
    name: str, role: str, prompt: str | list[str], steps: list[dict[str, Any]]
) -> dict[str, Any]:
    return {"name": name, "match": MARK[role], "match_prompt": prompt, "steps": steps}


def _ask(*questions: tuple[str, str, list[str]]) -> dict[str, Any]:
    """One AskUserQuestion call: (question, header, option labels) each."""
    qs = [
        {
            "question": q,
            "header": header,
            "multiSelect": False,
            "options": [{"label": o, "description": o} for o in options],
        }
        for q, header, options in questions
    ]
    return {"tool_use": {"name": "AskUserQuestion", "input": {"questions": qs}}}


def _workflow(args: dict[str, Any]) -> dict[str, Any]:
    return {"tool_use": {"name": "Workflow", "input": {"name": "grind-run", "args": args}}}


# ---- helpers -------------------------------------------------------------------


def _text(value: Any) -> str:
    """Every string inside `value`, joined: a request's raw prompt text."""
    if isinstance(value, str):
        return value
    if isinstance(value, dict):
        return "\n".join(_text(v) for v in value.values())
    if isinstance(value, list):
        return "\n".join(_text(v) for v in value)
    return ""


def _prompt(result: RunResult, role: str) -> str:
    return _text(result.first_request(role).get("messages"))


def _first(result: RunResult) -> dict[str, int]:
    """Each scripted role's first request number, i.e. the order agents started."""
    seen: dict[str, int] = {}
    for r in result.requests:
        seen[r["role"]] = min(seen.get(r["role"], r["n"]), r["n"])
    return seen


def _no_notes(result: RunResult) -> None:
    notes = [(r["role"], r["note"]) for r in result.requests if r.get("note")]
    assert not notes, notes


def _told(result: RunResult) -> str:
    seen = [r["messages"] for r in result.requests if r["role"] == "main"]
    return json.dumps(seen, ensure_ascii=False) + result.stdout


def _calls(h: Harness, *head: str) -> list[list[str]]:
    return [c for c in h.read_gh_state().get("calls", []) if c[: len(head)] == list(head)]


def _pr(h: Harness, number: int) -> dict[str, Any]:
    return next(p for p in h.read_gh_state()["prs"] if p["number"] == number)


def _issues(h: Harness) -> dict[str, dict[str, Any]]:
    return h.read_gh_state()["issues"]


def _origin(h: Harness, rev: str) -> str:
    return h.git("rev-parse", rev, cwd=h.origin)


def _origin_heads(h: Harness) -> list[str]:
    out = h.git("for-each-ref", "--format=%(refname:short)", "refs/heads", cwd=h.origin)
    return out.split()


def _run_path(h: Harness) -> Path:
    return Path(h.repo) / ".clud" / "grind" / "run.json"


def _write_run(h: Harness, facts: dict[str, Any]) -> None:
    run = _run_path(h)
    run.parent.mkdir(parents=True, exist_ok=True)
    run.write_text(json.dumps(facts), encoding="utf-8")


def _questions_then_none(result: RunResult, asked: int) -> None:
    """`asked` AskUserQuestion calls before prework, and none from anyone after."""
    assert len(result.questions_before("grind-prework")) == asked, result.hooks
    seen_prework = False
    for record in result.hooks:
        if record.get("agent_type") == "grind-prework":
            seen_prework = True
        if seen_prework:
            assert record.get("tool_name") != "AskUserQuestion", record
    assert seen_prework, "prework never ran"


def _all_goal_roles_ran(result: RunResult, goals: list[str]) -> None:
    first = _first(result)
    for g in goals:
        for role in ("planner", "worker", "reviewer", "integrator", "lander"):
            assert f"{role}:{g}" in first, (role, g, sorted(first))
        assert first["prework"] < first[f"planner:{g}"], (g, first)
        assert (
            first[f"planner:{g}"]
            < first[f"worker:{g}"]
            < first[f"reviewer:{g}"]
            < first[f"integrator:{g}"]
            < first[f"lander:{g}"]
        ), (g, first)


# ---- scripted roles ------------------------------------------------------------


def _prework(meta: str, run_id: str) -> dict[str, Any]:
    url = f"https://github.com/o/r/issues/{meta}#issuecomment-1000"
    steps = [
        _bash(f"gh issue comment {meta} --repo o/r --body '{PLAN_MARKER}{run_id} -->'"),
        _after(OK, _structured({"posted": True, "plan_url": url, "part_urls": [url]})),
    ]
    return _role("prework", "prework", "/grind-prework", steps)


def _bug_roles(
    h: Harness, goal: str, pr: int, *, problems: list[dict[str, Any]] | None = None
) -> list[dict[str, Any]]:
    """One bug goal: worked in the user's checkout, PR into main with `Closes #N`."""
    branch = f"grind/{goal}"
    repo = str(h.repo)
    at = f"Goal {goal}:"
    plan = {
        "checkout": repo,
        "branch": branch,
        "depends_on": [],
        "verify": "true",
        "tasks": [{"id": "t1", "files": [f"{goal}.txt"], "instructions": f"fix {goal}"}],
    }
    write = {"file_path": f"{repo}/{goal}.txt", "content": f"{goal}\n"}
    work: dict[str, Any] = {"files_touched": [f"{goal}.txt"], "summary": "fixed"}
    if problems:
        work["problems"] = problems
    git = f"git -C {repo}"
    push = (
        f"{git} switch -q main && {git} switch -q -c {branch} && "
        f"{git} add {goal}.txt && {git} commit -q -m 'fix: #{goal}' && "
        f"{git} push -q -u origin {branch} && "
        f"gh pr create --title 'fix #{goal}' --head {branch} --base main --body 'Closes #{goal}'"
    )
    url = f"https://github.com/o/r/pull/{pr}"
    return [
        _role(f"planner:{goal}", "planner", at, [_structured(plan)]),
        _role(
            f"worker:{goal}",
            "worker",
            at,
            [
                {"tool_use": {"name": "Write", "input": write}},
                _after(OK, _structured(work)),
            ],
        ),
        _role(
            f"reviewer:{goal}", "reviewer", at, [_structured({"approved": True, "summary": "ok"})]
        ),
        _role(
            f"integrator:{goal}",
            "integrator",
            at,
            [_bash(push), _after(OK, _structured({"pushed": True, "pr_url": url, "summary": "p"}))],
        ),
        _role(
            f"lander:{goal}",
            "lander",
            [at, "Fix rounds used: 0 of"],
            [
                _bash(f"gh pr merge {pr} --admin --squash"),
                _after(OK, _structured({"status": "merged", "summary": "green"})),
            ],
        ),
    ]


def _goals(ids: list[str]) -> list[dict[str, str]]:
    return [{"id": g, "title": f"child {g}", "brief": f"do {g}"} for g in ids]


# ---- scenario 1: a meta issue of unrelated bugs --------------------------------

S1_META = "1"
S1_BUGS = ["2", "3", "4", "5"]
S1_RUN = "b1a5"
S1_PROBLEM = {
    "kind": "pre-existing-failure",
    "summary": "test_locale_parse fails on untouched main",
    "evidence": "fails on origin/main before any change",
}


def test_e2e_1_bugs_only_each_lands_on_main_and_the_meta_is_not_hand_closed(
    harness: Harness,
) -> None:
    h = harness
    h.write_gh_state(meta_with_sub_issues(4))
    _write_run(h, {"mode": "sequential", "meta": S1_META, "problem_reporting": "issue"})
    prs = {g: 101 + i for i, g in enumerate(S1_BUGS)}
    plan = {
        "schema": "grind-plan/v1",
        "run_id": S1_RUN,
        "meta": int(S1_META),
        "repo": "o/r",
        "main": "main",
        "mode": "sequential",
        "preflight": {"action": "none", "branch": "main"},
        "structure": "simple",
        "stages": [{"stage": "bugs", "base": "main", "children": S1_BUGS}],
        "deferred_groups": [],
        "problem_reporting": "issue",
        "models": {},
        "ci": False,
        "scripts": {},
        "rules": {"stuck_bug": "block_dependents_only", "no_overlap": "bugs_only"},
    }
    args = {
        "repo": str(h.repo),
        "main": "main",
        "mode": "sequential",
        "meta": S1_META,
        "plan": plan,
        "goals": _goals(S1_BUGS),
    }
    title = shlex.quote(f"{S1_PROBLEM['kind']}: {S1_PROBLEM['summary']}")
    body = shlex.quote(
        f"{S1_PROBLEM['evidence']} Refs #{S1_META} "
        f"<!-- grind:followup meta={S1_META} stage=bugs feature-pr=none -->"
    )
    followup = (
        f"gh issue create --repo o/r --title {title} --label {FOLLOWUP} --body {body}"
    )
    main_steps = _chain(
        [
            # Routing: native sub-issues, so no conversion question.
            _bash(f"gh api repos/o/r/issues/{S1_META}/sub_issues"),
            # The one question round: no merge-policy question (no feature stage).
            _ask(
                ("Run mode?", "Mode", ["Sequential", "Parallel"]),
                ("Where do problems go?", "Problems", ["New issue per problem", "Comment"]),
            ),
            _after(_saw("New issue per problem"), _workflow(args)),
            _after(_saw(S1_PROBLEM["summary"]), _bash(followup)),
            {"text": "GRIND_DONE: 4 bugs merged, 1 follow-up filed"},
        ]
    )
    roles: list[dict[str, Any]] = [_prework(S1_META, S1_RUN)]
    for g in S1_BUGS:
        roles += _bug_roles(h, g, prs[g], problems=[S1_PROBLEM] if g == "3" else None)
    script = {"default_text": "OK", "roles": [*roles, {"name": "main", "steps": main_steps}]}
    answers = {"Run mode?": "Sequential", "Where do problems go?": "New issue per problem"}
    result = h.run("start grind", script, timeout=900, answers=answers)
    assert result.returncode == 0, result.stdout[-3000:]
    _no_notes(result)

    # Order: one question round, then prework, then each bug end to end in turn.
    _questions_then_none(result, 1)
    _all_goal_roles_ran(result, S1_BUGS)
    first = _first(result)
    for a, b in zip(S1_BUGS, S1_BUGS[1:]):
        assert first[f"lander:{a}"] < first[f"planner:{b}"], (a, b, first)

    # Every bug PR goes straight into main with Closes, and its merge closed it.
    issues = _issues(h)
    for g in S1_BUGS:
        pr = _pr(h, prs[g])
        assert pr["head"] == f"grind/{g}" and pr["base"] == "main", pr
        assert f"Closes #{g}" in pr["body"] and pr["state"] == "MERGED", pr
        assert issues[g]["state"] == "closed", (g, issues[g])
        assert issues[g]["closed_by"] == {"kind": "pr", "pr": prs[g]}, (g, issues[g])

    # No feature machinery at any point.
    state = h.read_gh_state()
    assert not [p for p in state["prs"] if p.get("draft") or p["base"] != "main"], state["prs"]
    assert not [b for b in _origin_heads(h) if b.startswith("grind/meta-")], _origin_heads(h)
    assert not (Path(h.repo) / ".clud" / "grind" / "worktrees" / "feature").exists()

    # The meta issue is left for reconcile: open, never closed by a command.
    assert issues[S1_META]["state"] == "open", issues[S1_META]
    assert not _calls(h, "issue", "close"), state["calls"]
    plan_comments = [c for c in issues[S1_META]["comments"] if PLAN_MARKER in c["body"]]
    assert len(plan_comments) == 1, issues[S1_META]["comments"]

    # The problem became exactly one follow-up issue, never a sub-issue.
    followups = {n: i for n, i in issues.items() if FOLLOWUP in i.get("labels", [])}
    assert list(followups) == ["105"], followups
    assert f"Refs #{S1_META}" in followups["105"]["body"]
    assert followups["105"]["parent"] is None
    assert 105 not in [s["number"] for s in issues[S1_META]["sub_issues"]]
    assert len(_calls(h, "issue", "create")) == 1


# ---- scenario 2: a prompt, prerequisite bugs, a dirty checkout, auto merge -----

S2_META = "6"
S2_BUGS = ["7", "8"]
S2_FEATURES = ["9", "10", "11"]
S2_RUN = "2b7c"
S2_FEATURE = f"grind/meta-{S2_META}-{S2_RUN}"
S2_FEATURE_PR = 101
S2_STASH = f"grind-{S2_RUN}"
S2_PROBLEM = {
    "kind": "doc-gap",
    "summary": "OAuth callback URL is undocumented",
    "evidence": "README has no callback section",
    "related_issue": "9",
}
S2_CHILDREN = [
    ("7", "fix: session cookie is not refreshed"),
    ("8", "fix: login redirect drops the return URL"),
    ("9", "feat: OAuth provider config"),
    ("10", "feat: OAuth callback endpoint"),
    ("11", "feat: OAuth login button (needs the redirect fix)"),
]


def _wt(h: Harness) -> Path:
    return Path(h.repo) / ".clud" / "grind" / "worktrees" / "feature"


def _s2_closes(goal: str) -> str:
    done = S2_FEATURES[: S2_FEATURES.index(goal) + 1]
    return " ".join(f"Closes #{n}" for n in [S2_META, *done])


def _feature_roles(h: Harness, goal: str, pr: int, *, merge_main: bool) -> list[dict[str, Any]]:
    """One feature goal, worked in the feature worktree, PR into the feature branch."""
    branch = f"grind/{goal}"
    at = f"Goal {goal}:"
    checkout = str(_wt(h))
    plan = {
        "checkout": checkout,
        "branch": branch,
        "depends_on": [],
        "verify": "true",
        "tasks": [{"id": "t1", "files": [f"{goal}.txt"], "instructions": f"build {goal}"}],
    }
    write = {"file_path": f"{checkout}/{goal}.txt", "content": f"{goal}\n"}
    work: dict[str, Any] = {"files_touched": [f"{goal}.txt"], "summary": "built"}
    if goal == S2_PROBLEM["related_issue"]:
        work["problems"] = [S2_PROBLEM]
    git = f"git -C {checkout}"
    steps = [f"{git} fetch -q origin"]
    if merge_main:
        steps += [
            f"{git} merge -q --no-ff origin/main -m 'merge main into {S2_FEATURE}'",
            f"{git} push -q origin HEAD:{S2_FEATURE}",
        ]
    steps += [
        f"{git} switch -q -c {branch} origin/{S2_FEATURE}",
        f"{git} add {goal}.txt",
        f"{git} commit -q -m 'feat: #{goal}'",
        f"{git} push -q -u origin {branch}",
        f"gh pr create --title 'feat #{goal}' --head {branch} --base {S2_FEATURE} "
        f"--body 'Refs #{goal}'",
        f"gh pr edit {S2_FEATURE_PR} --body '{_s2_closes(goal)}'",
    ]
    url = f"https://github.com/o/r/pull/{pr}"
    return [
        _role(f"planner:{goal}", "planner", at, [_structured(plan)]),
        _role(
            f"worker:{goal}",
            "worker",
            at,
            [
                {"tool_use": {"name": "Write", "input": write}},
                _after(OK, _structured(work)),
            ],
        ),
        _role(
            f"reviewer:{goal}", "reviewer", at, [_structured({"approved": True, "summary": "ok"})]
        ),
        _role(
            f"integrator:{goal}",
            "integrator",
            at,
            [
                _bash(" && ".join(steps)),
                _after(OK, _structured({"pushed": True, "pr_url": url, "summary": "p"})),
            ],
        ),
        _role(
            f"lander:{goal}",
            "lander",
            [at, "Fix rounds used: 0 of"],
            [
                _bash(f"gh pr merge {pr} --merge"),
                _after(OK, _structured({"status": "merged", "summary": "green"})),
            ],
        ),
    ]


def _feature_lander() -> dict[str, Any]:
    steps = [
        _bash(f"gh pr ready {S2_FEATURE_PR}"),
        _after(OK, _bash(f"gh pr merge {S2_FEATURE_PR} --merge")),
        _after(OK, _structured({"status": "merged", "summary": "feature merged"})),
    ]
    return _role("lander:feature", "lander", "for the FEATURE PR", steps)


def test_e2e_2_prompt_feature_auto_merge_bugs_first_then_one_feature_merge_commit(
    harness: Harness,
) -> None:
    h = harness
    h.write_gh_state(mixed())
    before = {n: dict(i) for n, i in h.read_gh_state()["issues"].items()}
    # The feature worktree lives inside the repo; keep it (and run.json) out of
    # the stash, as a real repo's ignore rules would.
    exclude = Path(h.repo) / ".git" / "info" / "exclude"
    exclude.write_text(exclude.read_text(encoding="utf-8") + ".clud/\n", encoding="utf-8")
    feature_before = h.git("rev-parse", "main")
    h.git("branch", S2_FEATURE, "main")
    h.git("push", "-q", "-u", "origin", S2_FEATURE)
    h.git("worktree", "add", "-q", str(_wt(h)), S2_FEATURE)
    # main moves after the feature branch was cut.
    h.git("commit", "-q", "--allow-empty", "-m", "main moved")
    h.git("push", "-q", "origin", "main")
    main_after = h.git("rev-parse", "main")
    # A dirty checkout: one modified tracked file, one untracked file.
    readme = Path(h.repo) / "README.md"
    readme.write_text(readme.read_text(encoding="utf-8") + "my local edit\n", encoding="utf-8")
    notes = Path(h.repo) / "notes.txt"
    notes.write_text("scratch\n", encoding="utf-8")
    _write_run(
        h,
        {
            "mode": "sequential",
            "meta": S2_META,
            "feature": {
                "branch": S2_FEATURE,
                "worktree": str(_wt(h)),
                "pr": f"https://github.com/o/r/pull/{S2_FEATURE_PR}",
            },
            "feature_merge": "auto",
            "problem_reporting": "comment",
            "preflight": {"action": "stash", "stash": S2_STASH, "branch": "main"},
        },
    )
    bug_prs = {"7": 102, "8": 103}
    feature_prs = {"9": 104, "10": 105, "11": 106}
    plan = {
        "schema": "grind-plan/v1",
        "run_id": S2_RUN,
        "meta": int(S2_META),
        "repo": "o/r",
        "main": "main",
        "mode": "sequential",
        "preflight": {"action": "stash", "stash": S2_STASH, "branch": "main"},
        "structure": "simple",
        "stages": [
            {"stage": "bugs", "base": "main", "children": S2_BUGS},
            {
                "stage": "feature",
                "group": "oauth login",
                "sub_meta": None,
                "branch": S2_FEATURE,
                "base": S2_FEATURE,
                "children": S2_FEATURES,
                "depends_on_bugs": {"11": ["8"]},
            },
        ],
        "deferred_groups": [],
        "feature_merge": "auto",
        "problem_reporting": "comment",
        "models": {},
        "ci": False,
        "scripts": {},
        "rules": {"stuck_bug": "block_dependents_only", "no_overlap": "bugs_only"},
    }
    args = {
        "repo": str(h.repo),
        "main": "main",
        "mode": "sequential",
        "meta": S2_META,
        "plan": plan,
        "goals": [{"id": n, "title": t, "brief": t} for n, t in S2_CHILDREN],
        "feature": {
            "branch": S2_FEATURE,
            "worktree": str(_wt(h)),
            "pr": f"https://github.com/o/r/pull/{S2_FEATURE_PR}",
        },
        "feature_merge": "auto",
    }
    # Routing: the prompt becomes meta #6 and five children, attached and verified.
    route = [
        "gh issue create --repo o/r --title 'meta: OAuth login' --body 'Tracked as sub-issues.'",
        *[
            f"gh issue create --repo o/r --title {shlex.quote(t)} --body {shlex.quote(t)}"
            for _, t in S2_CHILDREN
        ],
        *[
            f"gh api -X POST repos/o/r/issues/{S2_META}/sub_issues -F sub_issue_id={n}"
            for n, _ in S2_CHILDREN
        ],
        f"gh api repos/o/r/issues/{S2_META}/sub_issues",
    ]
    repo = str(h.repo)
    draft = (
        f"gh pr create --draft --title 'grind: meta {S2_META}' --head {S2_FEATURE} "
        f"--base main --body 'Closes #{S2_META}'"
    )
    comment = "gh issue comment 9 --body " + shlex.quote(
        f"grind problem ({S2_PROBLEM['kind']}): {S2_PROBLEM['summary']}. {S2_PROBLEM['evidence']}"
    )
    main_steps = _chain(
        [
            _bash(" && ".join(route)),
            _ask(
                (
                    "Your checkout has uncommitted changes. What should grind do?",
                    "Dirty repo",
                    [
                        "Stash it",
                        "Commit to a WIP branch",
                        "Carry into the grind worktree",
                        "Abort",
                    ],
                ),
                (
                    "What happens to the feature PR when every feature goal has landed?",
                    "Merge",
                    ["Auto-merge when done (Recommended)", "Decide later", "Comment only"],
                ),
                ("Where do problems go?", "Problems", ["New issue per problem", "Comment"]),
            ),
            _after(
                _saw("Stash it"), _bash(f"git -C {repo} stash push -q -u -m {S2_STASH}")
            ),
            _bash(draft),
            _workflow(args),
            _after(_saw(S2_PROBLEM["summary"]), _bash(comment)),
            # Finish: back to the starting branch, stash restored.
            _bash(f"git -C {repo} switch -q main && git -C {repo} stash pop -q"),
            {"text": "GRIND_DONE: 2 bugs merged, feature PR merged"},
        ]
    )
    roles: list[dict[str, Any]] = [_prework(S2_META, S2_RUN), _feature_lander()]
    for g in S2_BUGS:
        roles += _bug_roles(h, g, bug_prs[g])
    for g in S2_FEATURES:
        roles += _feature_roles(h, g, feature_prs[g], merge_main=g == S2_FEATURES[0])
    script = {"default_text": "OK", "roles": [*roles, {"name": "main", "steps": main_steps}]}
    answers = {
        "Your checkout has uncommitted changes. What should grind do?": "Stash it",
        "What happens to the feature PR when every feature goal has landed?": (
            "Auto-merge when done (Recommended)"
        ),
        "Where do problems go?": "Comment",
    }
    result = h.run(
        "/grind add OAuth login; the session cookie bug and the redirect bug must be fixed first",
        script,
        timeout=1200,
        answers=answers,
    )
    assert result.returncode == 0, result.stdout[-3000:]
    _no_notes(result)

    # Routing made meta #6 with five native sub-issues; the backlog is untouched.
    issues = _issues(h)
    assert {s["number"] for s in issues[S2_META]["sub_issues"]} == {7, 8, 9, 10, 11}
    assert all(issues[n]["parent"] == int(S2_META) for n, _ in S2_CHILDREN)
    for n, old in before.items():
        assert (issues[n]["title"], issues[n]["body"]) == (old["title"], old["body"]), n

    # Order: questions, prework, every bug landed, then the feature goals, then
    # the feature PR.
    _questions_then_none(result, 1)
    _all_goal_roles_ran(result, [*S2_BUGS, *S2_FEATURES])
    first = _first(result)
    last_bug = max(first[f"lander:{b}"] for b in S2_BUGS)
    assert all(last_bug < first[f"planner:{f}"] for f in S2_FEATURES), first
    assert first["lander:feature"] > max(first[f"lander:{f}"] for f in S2_FEATURES), first
    calls = h.read_gh_state()["calls"]
    drafted = next(i for i, c in enumerate(calls) if c[:2] == ["pr", "create"] and "--draft" in c)
    merged = next(i for i, c in enumerate(calls) if c[:2] == ["pr", "merge"])
    assert drafted < merged, calls

    # Bug stage: PRs into main, each closing its own issue.
    for g in S2_BUGS:
        pr = _pr(h, bug_prs[g])
        assert pr["base"] == "main" and f"Closes #{g}" in pr["body"], pr
        assert pr["state"] == "MERGED", pr
        assert issues[g]["closed_by"] == {"kind": "pr", "pr": bug_prs[g]}, (g, issues[g])
        assert "Base: origin/main" in _prompt(result, f"integrator:{g}")

    # Feature stage: goal PRs into the feature branch with Refs, never Closes.
    for g in S2_FEATURES:
        pr = _pr(h, feature_prs[g])
        assert pr["base"] == S2_FEATURE and pr["state"] == "MERGED", pr
        assert f"Refs #{g}" in pr["body"] and "Closes" not in pr["body"], pr
        assert f"Base: origin/{S2_FEATURE}" in _prompt(result, f"integrator:{g}")

    # The feature PR: readied, merge-committed without --admin, and the only
    # closer of the meta issue and every feature part.
    feature = _pr(h, S2_FEATURE_PR)
    assert feature["base"] == "main" and feature["state"] == "MERGED", feature
    assert feature["draft"] is False and feature["merge_method"] == "merge", feature
    assert feature["admin"] is False, feature
    ready = _calls(h, "pr", "ready", str(S2_FEATURE_PR))
    merge = _calls(h, "pr", "merge", str(S2_FEATURE_PR))
    assert ready and merge and calls.index(ready[0]) < calls.index(merge[0]), calls
    assert all("--admin" not in c and "--squash" not in c for c in merge), merge
    for n in (S2_META, *S2_FEATURES):
        assert f"Closes #{n}" in feature["body"], feature["body"]
        assert issues[n]["state"] == "closed", (n, issues[n])
        assert issues[n]["closed_by"] == {"kind": "pr", "pr": S2_FEATURE_PR}, (n, issues[n])
    assert not _calls(h, "issue", "close"), calls

    # main moving was merged into the feature branch, never rebased.
    assert _origin(h, "main") == main_after
    tip = _origin(h, S2_FEATURE)
    parents = h.git("rev-list", "--parents", "-n", "1", tip, cwd=h.origin).split()
    assert parents[1:] == [feature_before, main_after], parents

    # Problems went where the user chose: one comment on the related child.
    assert not [i for i in issues.values() if FOLLOWUP in i.get("labels", [])]
    on_child = [c for c in issues["9"]["comments"] if S2_PROBLEM["summary"] in c["body"]]
    assert len(on_child) == 1, issues["9"]["comments"]

    # The public plan leaves out local-only details.
    pre = _prompt(result, "prework")
    assert '"preflight": "handled"' in pre, pre[-3000:]
    assert S2_STASH not in pre

    # Finish restored the stash and the starting branch.
    assert h.git("branch", "--show-current") == "main"
    assert h.git("stash", "list") == ""
    assert "my local edit" in readme.read_text(encoding="utf-8")
    assert notes.read_text(encoding="utf-8") == "scratch\n"


# ---- scenario 3: a big messy epic, regrouped, feature decided later ------------

S3_TOP = "1"
S3_RUN = "e3d4"
S3_BUGS = ["12", "13", "14", "15"]
S3_DOCS = ["2", "3", "4", "5"]
S3_CLI = ["6", "7", "8", "9"]
S3_REPORT = "GRIND_DONE: 4 bugs merged; features docs (#10) and cli (#11) left for you to decide"
S3_GROUPS = [("docs", [int(n) for n in S3_DOCS]), ("cli", [int(n) for n in S3_CLI])]


def _move(state: dict[str, Any], child: int, parent: int) -> None:
    """Reparent `child` under `parent`, both directions kept consistent."""
    issues = state["issues"]
    old = issues[str(child)]["parent"]
    if old is not None:
        subs = issues[str(old)]["sub_issues"]
        issues[str(old)]["sub_issues"] = [s for s in subs if s["number"] != child]
    issues[str(child)]["parent"] = parent
    issues[str(parent)]["sub_issues"].append({"number": child, "state": "open"})


def _messy_epic() -> dict[str, Any]:
    """`regroupable()` (docs #2-#5, cli #6-#9 under #1), plus the user's own
    sub-metas #10 (holding #2, #3) and #11 (holding #6, #7), and bugs #12-#15."""
    state = regroupable()
    issues = state["issues"]
    issues["10"] = _issue("meta: my docs notes", "Docs things I noticed.", parent=1)
    issues["11"] = _issue("meta: my cli notes", "CLI things I noticed.", parent=1)
    for n in (10, 11):
        issues[S3_TOP]["sub_issues"].append({"number": n, "state": "open"})
    for child, parent in ((2, 10), (3, 10), (6, 11), (7, 11)):
        _move(state, child, parent)
    for n in S3_BUGS:
        issues[n] = _issue(f"bug {n}", f"fix {n}", parent=1)
        issues[S3_TOP]["sub_issues"].append({"number": int(n), "state": "open"})
    return state


def _s3_feature_stage(name: str, sub: int, kids: list[str]) -> dict[str, Any]:
    branch = f"grind/meta-{sub}-{S3_RUN}"
    return {
        "stage": "feature",
        "group": name,
        "sub_meta": sub,
        "branch": branch,
        "base": branch,
        "children": kids,
    }


def test_e2e_3_regrouped_epic_decide_later_runs_only_bugs_and_leaves_features_open(
    harness: Harness,
) -> None:
    h = harness
    h.write_gh_state(_messy_epic())
    before = h.read_gh_state()
    cmds, undo, where = _regroup(before, S3_GROUPS)
    # The user's two sub-metas are reused in place; nothing new is created.
    assert where == {"docs": 10, "cli": 11}, where
    prs = {g: 101 + i for i, g in enumerate(S3_BUGS)}
    plan = {
        "schema": "grind-plan/v1",
        "run_id": S3_RUN,
        "meta": int(S3_TOP),
        "repo": "o/r",
        "main": "main",
        "mode": "sequential",
        "preflight": {"action": "none", "branch": "main"},
        "structure": "meta_of_metas",
        "stages": [
            {"stage": "bugs", "base": "main", "children": S3_BUGS},
            _s3_feature_stage("docs", 10, S3_DOCS),
            _s3_feature_stage("cli", 11, S3_CLI),
        ],
        # "Decide later" on the feature pick: no feature runs this time.
        "deferred_groups": [
            {"group": "docs", "sub_meta": 10, "children": S3_DOCS},
            {"group": "cli", "sub_meta": 11, "children": S3_CLI},
        ],
        "feature_merge": "later",
        "problem_reporting": "issue",
        "models": {},
        "ci": False,
        "scripts": {},
        "rules": {"stuck_bug": "block_dependents_only", "no_overlap": "bugs_only"},
    }
    run = {
        "mode": "sequential",
        "meta": S3_TOP,
        "undo": undo,
        "problem_reporting": "issue",
        "waiting_on_pr": None,
    }
    args = {
        "repo": str(h.repo),
        "main": "main",
        "mode": "sequential",
        "meta": S3_TOP,
        "plan": plan,
        "goals": _goals([*S3_BUGS, *S3_DOCS, *S3_CLI]),
    }
    regroup_q = "Regroup #1 into a meta of metas? bugs: #12 #13 #14 #15 · F1 docs · F2 cli"
    pick_q = "Which feature does this run do?"
    main_steps = _chain(
        [
            _bash(f"gh api repos/o/r/issues/{S3_TOP}/sub_issues"),
            _ask(
                (regroup_q, "Regroup", ["Regroup", "Keep as is (simple schedule)"]),
                (pick_q, "Feature", ["docs (Recommended)", "cli", "Decide later"]),
            ),
            _after(_saw("Decide later"), _write(_run_path(h), json.dumps(run, indent=1))),
            *[_bash(c) for c in cmds],
            _workflow(args),
            _after(_saw("deferred group docs"), {"text": S3_REPORT}),
        ]
    )
    roles: list[dict[str, Any]] = [_prework(S3_TOP, S3_RUN)]
    for g in S3_BUGS:
        roles += _bug_roles(h, g, prs[g])
    script = {"default_text": "OK", "roles": [*roles, {"name": "main", "steps": main_steps}]}
    result = h.run(
        "start grind",
        script,
        timeout=900,
        answers={regroup_q: "Regroup", pick_q: "Decide later"},
    )
    assert result.returncode == 0, result.stdout[-3000:]
    _no_notes(result)

    # Order: the regroup and the feature pick were asked once, before prework.
    _questions_then_none(result, 1)
    _all_goal_roles_ran(result, S3_BUGS)
    after = h.read_gh_state()
    issues = after["issues"]

    # Regrouped in place: the user's sub-metas became F1/F2, old text kept.
    assert not _calls(h, "issue", "create"), after["calls"]
    for n, name in ((10, "docs"), (11, "cli")):
        issue = issues[str(n)]
        assert issue["title"] == f"grind: {name}", issue
        assert issue["body"].startswith(V1), issue
        old = before["issues"][str(n)]["body"]
        assert f"{PREVIOUS}\n\n{old}\n\n</details>" in issue["body"], issue
        assert issue["state"] == "open", issue
    assert {s["number"] for s in issues["10"]["sub_issues"]} == {2, 3, 4, 5}
    assert {s["number"] for s in issues["11"]["sub_issues"]} == {6, 7, 8, 9}
    assert {s["number"] for s in issues[S3_TOP]["sub_issues"]} == {10, 11, 12, 13, 14, 15}
    saved = json.loads(_run_path(h).read_text(encoding="utf-8"))["undo"]
    assert {(u["op"], u["issue"]) for u in saved} >= {
        ("rewrite", 10),
        ("rewrite", 11),
        ("reparent", 4),
        ("reparent", 5),
        ("reparent", 8),
        ("reparent", 9),
    }, saved

    # Only bugs ran: each landed on main and closed its own issue.
    for g in S3_BUGS:
        pr = _pr(h, prs[g])
        assert pr["base"] == "main" and f"Closes #{g}" in pr["body"], pr
        assert pr["state"] == "MERGED", pr
        assert issues[g]["closed_by"] == {"kind": "pr", "pr": prs[g]}, (g, issues[g])
        assert issues[g]["parent"] == int(S3_TOP)
    assert {p["number"] for p in after["prs"]} == set(prs.values()), after["prs"]

    # Both features were deferred: no agent saw them, no branch, no PR, still open.
    for g in [*S3_DOCS, *S3_CLI]:
        seen = [
            r["role"]
            for r in result.requests
            if r["role"] != "main" and f"Goal {g}:" in _text(r.get("messages"))
        ]
        assert not seen, (g, seen)
        assert issues[g]["state"] == "open", (g, issues[g])
    assert not [b for b in _origin_heads(h) if b.startswith("grind/meta-")], _origin_heads(h)
    assert not [p for p in after["prs"] if p.get("draft")], after["prs"]
    told = _told(result)
    assert "deferred group docs" in told and "deferred group cli" in told, told[-3000:]

    # The top meta issue carries the plan and is left for reconcile.
    assert issues[S3_TOP]["state"] == "open"
    assert not _calls(h, "issue", "close"), after["calls"]
    plans = [c for c in issues[S3_TOP]["comments"] if PLAN_MARKER in c["body"]]
    assert len(plans) == 1, issues[S3_TOP]["comments"]
