"""`clud grind reconcile` against the fake GitHub (#1393 §3).

A two-goal feature world: meta #100 with children #102 and #103, feature PR
#101 (`grind/meta-100-1f3a` -> `main`). Every issue the run touched carries
the `grind:on-feature` label and a `<!-- grind:v1 feature-pr=#101 ... -->`
marker comment. Reconcile reads only GitHub state, so these tests run the
built `clud` directly; no scripted agent is needed.

Cases:
1. the feature PR is closed unmerged -> every issue open and unlabelled;
2. a goal issue closed by hand mid-run (closer none) -> reopened;
3. the feature PR merged into `main` -> every issue closed and unlabelled;
4. a converted original issue is protected like the meta issue (S4);
5. a child closed by hand before its goal landed (no label yet) is reopened
   through the meta issue's sub-issues;
6. an open feature PR waiting for a required review is reported (F5).
"""

from __future__ import annotations

from typing import Any

from tests import process
from tests.harness.harness import Harness
from tests.harness.worlds import _issue, _world

META = "100"
RUN_ID = "1f3a"
FEATURE = f"grind/meta-{META}-{RUN_ID}"
PR = "101"
GOALS = ["102", "103"]
LABEL = "grind:on-feature"
ALL = (META, *GOALS)


def _marker(goal: str | None) -> str:
    goal_pr = f" goal-pr=#{goal}" if goal else ""
    return f"<!-- grind:v1 feature-pr=#{PR} branch={FEATURE}{goal_pr} run={RUN_ID} -->"


def _seed(h: Harness, *, pr_state: str = "OPEN", closed_by_hand: str | None = None) -> None:
    issues = {int(META): _issue("meta: auth rework", "Tracked as sub-issues.", labels=[LABEL])}
    for n in GOALS:
        issues[int(n)] = _issue(f"child {n}", f"do {n}", labels=[LABEL], parent=int(META))
    state = _world(issues)
    cid = 2000
    for n in ALL:
        goal = n if n != META else None
        state["issues"][n]["comments"].append({"id": cid, "body": _marker(goal)})
        cid += 1
    if closed_by_hand is not None:
        state["issues"][closed_by_hand]["state"] = "closed"
        state["issues"][closed_by_hand]["closed_by"] = {"kind": "user", "pr": None}
    state["prs"] = [
        {
            "number": int(PR),
            "head": FEATURE,
            "state": pr_state,
            "title": f"grind: meta {META}",
            "base": "main",
            "draft": pr_state == "OPEN",
            "body": " ".join(f"Closes #{n}" for n in ALL),
        }
    ]
    h.write_gh_state(state)


def _reconcile(h: Harness) -> Any:
    result = process.run(
        [str(h.clud), "grind", "reconcile"],
        cwd=str(h.repo),
        capture_output=True,
        text=True,
        env=h.env(),
        timeout=120,
    )
    out = (result.stdout or "") + (result.stderr or "")
    assert result.returncode == 0, out[-3000:]
    return result


def _issues(h: Harness) -> dict[str, Any]:
    return h.read_gh_state()["issues"]


def _merge_feature(h: Harness) -> None:
    """Merge the feature PR into `main` through `fake_gh`, as GitHub would."""
    gh = h.bin / "gh"
    for sub in (["pr", "ready", PR], ["pr", "merge", PR, "--merge"]):
        result = process.run(
            [str(gh), *sub], capture_output=True, text=True, env=h.env(), timeout=60
        )
        assert result.returncode == 0, (result.stdout, result.stderr)


def test_r1_feature_pr_closed_unmerged_reopens_and_unlabels(harness: Harness) -> None:
    _seed(harness, pr_state="CLOSED")
    _reconcile(harness)
    issues = _issues(harness)
    for n in ALL:
        assert issues[n]["state"] == "open", (n, issues[n])
        assert LABEL not in issues[n]["labels"], (n, issues[n])
        assert any(f"refs/pull/{PR}/head" in c["body"] for c in issues[n]["comments"]), issues[n]


def test_r2_goal_closed_by_hand_mid_run_is_reopened(harness: Harness) -> None:
    _seed(harness, closed_by_hand=GOALS[0])
    _reconcile(harness)
    issues = _issues(harness)
    goal = issues[GOALS[0]]
    assert goal["state"] == "open", goal
    assert LABEL in goal["labels"], goal
    assert any("Reopened by `clud grind reconcile`" in c["body"] for c in goal["comments"]), goal
    # The feature PR is still open and healthy: the others are untouched.
    for n in (META, GOALS[1]):
        assert issues[n]["state"] == "open", issues[n]
        assert LABEL in issues[n]["labels"], issues[n]


def test_r3_feature_pr_merged_into_default_closes_and_unlabels(harness: Harness) -> None:
    _seed(harness)
    _merge_feature(harness)
    _reconcile(harness)
    issues = _issues(harness)
    for n in ALL:
        assert issues[n]["state"] == "closed", (n, issues[n])
        assert LABEL not in issues[n]["labels"], (n, issues[n])


def test_reconcile_is_idempotent(harness: Harness) -> None:
    _seed(harness, pr_state="CLOSED")
    _reconcile(harness)
    before = _issues(harness)
    _reconcile(harness)
    assert _issues(harness) == before


def test_reconcile_is_idempotent_after_reopening(harness: Harness) -> None:
    _seed(harness, closed_by_hand=GOALS[0])
    _reconcile(harness)
    before = _issues(harness)
    _reconcile(harness)
    assert _issues(harness) == before
    reopens = [c for c in before[GOALS[0]]["comments"] if "Reopened by" in c["body"]]
    assert len(reopens) == 1, before[GOALS[0]]["comments"]


ORIGINAL = "99"


def _seed_original(h: Harness, *, closed_by_hand: bool) -> None:
    """Meta #100 converted from #99: #99 carries the label and the meta marker."""
    _seed(h)
    state = h.read_gh_state()
    original = _issue("the original multi-part issue", "Split into meta #100", labels=[LABEL])
    original["comments"] = [{"id": 2999, "body": _marker(None)}]
    if closed_by_hand:
        original["state"] = "closed"
        original["closed_by"] = {"kind": "user", "pr": None}
    state["issues"][ORIGINAL] = original
    state["prs"][0]["body"] += f" Closes #{ORIGINAL}"
    h.write_gh_state(state)


def test_s4_converted_original_is_reopened_early_and_closes_with_the_feature(
    harness: Harness,
) -> None:
    _seed_original(harness, closed_by_hand=True)
    _reconcile(harness)
    original = _issues(harness)[ORIGINAL]
    assert original["state"] == "open", original
    assert LABEL in original["labels"], original

    _merge_feature(harness)
    _reconcile(harness)
    issues = _issues(harness)
    for n in (ORIGINAL, *ALL):
        assert issues[n]["state"] == "closed", (n, issues[n])
        assert issues[n]["closed_by"] == {"kind": "pr", "pr": int(PR)}, (n, issues[n])
        assert LABEL not in issues[n]["labels"], (n, issues[n])


def test_unlabelled_child_closed_by_hand_mid_run_is_reopened(harness: Harness) -> None:
    """#103 has not landed yet (no label, no marker) when a person closes it."""
    _seed(harness)
    state = harness.read_gh_state()
    child = state["issues"][GOALS[1]]
    child["labels"] = []
    child["comments"] = []
    child["state"] = "closed"
    child["closed_by"] = {"kind": "user", "pr": None}
    for s in state["issues"][META]["sub_issues"]:
        if str(s["number"]) == GOALS[1]:
            s["state"] = "closed"
    harness.write_gh_state(state)
    _reconcile(harness)
    issues = _issues(harness)
    assert issues[GOALS[1]]["state"] == "open", issues[GOALS[1]]
    assert any(
        f"feature PR #{PR}" in c["body"] and "Reopened by" in c["body"]
        for c in issues[GOALS[1]]["comments"]
    ), issues[GOALS[1]]
    assert LABEL not in issues[GOALS[1]]["labels"]


def test_f5_open_feature_pr_waiting_for_review_is_reported(harness: Harness) -> None:
    _seed(harness)
    state = harness.read_gh_state()
    state["prs"][0].update({"draft": False, "reviews_required": True, "approved": False})
    harness.write_gh_state(state)
    result = _reconcile(harness)
    out = result.stdout or ""
    assert f"feature PR #{PR} ({FEATURE}): ready, waiting for review" in out, out
    for n in ALL:
        assert f"#{n}" in out, out
    # Reporting changes nothing on GitHub.
    issues = _issues(harness)
    assert all(issues[n]["state"] == "open" and LABEL in issues[n]["labels"] for n in ALL)
