"""Unit tests for the fake `gh` GitHub semantics used by /grind tests (#1402)."""

from __future__ import annotations

import json

import pytest

from tests.harness import fake_gh


@pytest.fixture
def world(tmp_path, monkeypatch):
    path = tmp_path / "gh.json"

    def make(state: dict | None = None):
        path.write_text(json.dumps(state or {"repo": "o/r", "issues": {}, "prs": []}))
        monkeypatch.setenv("FAKE_GH_STATE", str(path))
        return path

    make.path = path
    return make


def _state(world) -> dict:
    return json.loads(world.path.read_text())


def _run(capsys, *argv: str) -> tuple[int, str, str]:
    capsys.readouterr()
    code = fake_gh.main(list(argv))
    out = capsys.readouterr()
    return code, out.out, out.err


def test_legacy_state_shape_still_works(world, capsys):
    world(
        {
            "repo": "o/r",
            "issues": {
                "7": {
                    "title": "Meta",
                    "body": "b",
                    "state": "open",
                    "sub_issues": [{"number": 8, "state": "closed"}],
                }
            },
            "prs": [{"number": 1, "head": "feat/x", "state": "MERGED", "title": "t"}],
            "fail": False,
            "calls": [],
        }
    )
    code, out, _ = _run(capsys, "issue", "view", "7", "--json", "title,body,state")
    assert code == 0
    data = json.loads(out)
    assert (data["number"], data["title"], data["state"]) == (7, "Meta", "OPEN")
    code, out, _ = _run(capsys, "api", "repos/o/r/issues/7/sub_issues")
    assert code == 0
    assert json.loads(out) == [{"number": 8, "state": "closed"}]
    code, out, _ = _run(capsys, "pr", "list", "--head", "feat/x", "--state", "all")
    assert json.loads(out)[0]["number"] == 1
    assert len(_state(world)["calls"]) == 3


def test_create_comment_labels_and_list(world, capsys):
    world()
    code, out, _ = _run(capsys, "issue", "create", "--title", "A", "--body", "x", "--label", "bug")
    assert code == 0 and out.strip() == "https://github.com/o/r/issues/1"
    _run(capsys, "issue", "create", "--title", "B", "--body", "y", "--label", "a,b")
    code, out, _ = _run(capsys, "issue", "comment", "1", "--body", "hi")
    first = int(out.strip().rsplit("#issuecomment-", 1)[1])
    code, out, _ = _run(capsys, "issue", "comment", "1", "--body", "again")
    second = int(out.strip().rsplit("#issuecomment-", 1)[1])
    assert second != first

    _run(capsys, "issue", "edit", "1", "--add-label", "x,y", "--remove-label", "bug")
    code, out, _ = _run(capsys, "issue", "view", "1", "--json", "labels,comments")
    data = json.loads(out)
    assert [lbl["name"] for lbl in data["labels"]] == ["x", "y"]
    assert data["comments"] == [{"id": first, "body": "hi"}, {"id": second, "body": "again"}]

    code, out, _ = _run(capsys, "issue", "list", "--label", "a", "--json", "number")
    assert [i["number"] for i in json.loads(out)] == [2]
    _run(capsys, "issue", "close", "2")
    _, out, _ = _run(capsys, "issue", "list", "--state", "open", "--json", "number")
    assert [i["number"] for i in json.loads(out)] == [1]
    _, out, _ = _run(capsys, "issue", "list", "--state", "closed", "--json", "number")
    rows = json.loads(out)
    assert rows[0]["number"] == 2 and rows[0]["state"] == "CLOSED"
    _, out, _ = _run(capsys, "issue", "list", "--state", "all", "--json", "number")
    assert len(json.loads(out)) == 2


def test_close_and_reopen_record_closer(world, capsys):
    world({"issues": {"3": {"title": "t", "body": "", "state": "open"}}})
    code, _, _ = _run(capsys, "issue", "close", "3", "--comment", "done")
    assert code == 0
    issue = _state(world)["issues"]["3"]
    assert issue["state"] == "closed"
    assert issue["closed_by"]["kind"] == "command"
    assert issue["comments"][0]["body"] == "done"
    _run(capsys, "issue", "reopen", "3")
    issue = _state(world)["issues"]["3"]
    assert issue["state"] == "open" and issue["closed_by"] is None


def test_comment_patch_and_get(world, capsys):
    world({"issues": {"1": {"title": "t", "body": "", "state": "open"}}})
    _, out, _ = _run(capsys, "issue", "comment", "1", "--body", "old")
    cid = out.strip().rsplit("-", 1)[1]
    code, _, _ = _run(
        capsys, "api", "-X", "PATCH", f"repos/o/r/issues/comments/{cid}", "-f", "body=new"
    )
    assert code == 0
    code, out, _ = _run(capsys, "api", f"repos/o/r/issues/comments/{cid}")
    assert code == 0 and json.loads(out)["body"] == "new"


def _issues(n: int) -> dict:
    return {str(i): {"title": f"i{i}", "body": "", "state": "open"} for i in range(1, n + 1)}


def _add(capsys, parent: int, child: int, *extra: str):
    return _run(
        capsys,
        "api",
        "-X",
        "POST",
        f"repos/o/r/issues/{parent}/sub_issues",
        "-F",
        f"sub_issue_id={child}",
        *extra,
    )


def test_sub_issue_post_replace_one_parent_and_delete(world, capsys):
    world({"issues": _issues(3)})
    assert _add(capsys, 1, 3)[0] == 0
    code, _, err = _add(capsys, 2, 3)
    assert code == 1 and "422" in err
    assert _add(capsys, 2, 3, "-F", "replace_parent=true")[0] == 0
    state = _state(world)
    assert state["issues"]["1"]["sub_issues"] == []
    assert [s["number"] for s in state["issues"]["2"]["sub_issues"]] == [3]
    assert state["issues"]["3"]["parent"] == 2
    _, out, _ = _run(capsys, "api", "repos/o/r/issues/2/sub_issues")
    assert [s["number"] for s in json.loads(out)] == [3]
    code, _, _ = _run(
        capsys, "api", "-X", "DELETE", "repos/o/r/issues/2/sub_issue", "-F", "sub_issue_id=3"
    )
    assert code == 0
    state = _state(world)
    assert state["issues"]["2"]["sub_issues"] == [] and state["issues"]["3"]["parent"] is None


def test_sub_issue_hundred_limit(world, capsys):
    issues = _issues(102)
    issues["1"]["sub_issues"] = [{"number": i, "state": "open"} for i in range(2, 102)]
    for i in range(2, 102):
        issues[str(i)]["parent"] = 1
    world({"issues": issues})
    code, _, err = _add(capsys, 1, 102)
    assert code == 1 and "422" in err


def test_sub_issue_eight_level_limit(world, capsys):
    world({"issues": _issues(9)})
    for level in range(1, 8):
        assert _add(capsys, level, level + 1)[0] == 0
    code, _, err = _add(capsys, 8, 9)
    assert code == 1 and "422" in err


def test_pr_create_draft_ready_edit(world, capsys):
    world()
    code, out, _ = _run(
        capsys, "pr", "create", "--head", "f", "--title", "t", "--body", "b", "--draft"
    )
    number = out.strip().rsplit("/", 1)[1]
    _, out, _ = _run(capsys, "pr", "view", number, "--json", "isDraft,baseRefName,body")
    data = json.loads(out)
    assert data["isDraft"] is True and data["baseRefName"] == "main" and data["body"] == "b"
    assert _run(capsys, "pr", "merge", number, "--squash")[0] == 1
    _run(capsys, "pr", "ready", number)
    _run(capsys, "pr", "edit", number, "--body", "b2")
    _, out, _ = _run(capsys, "pr", "view", number, "--json", "isDraft,body")
    assert json.loads(out)["isDraft"] is False and json.loads(out)["body"] == "b2"


def test_merge_records_method_and_admin_and_requires_review(world, capsys):
    world(
        {
            "issues": {},
            "prs": [
                {"number": 1, "head": "a", "state": "OPEN", "title": "t"},
                {
                    "number": 2,
                    "head": "b",
                    "state": "OPEN",
                    "title": "t",
                    "reviews_required": True,
                    "approved": False,
                },
            ],
        }
    )
    assert _run(capsys, "pr", "merge", "1", "--rebase")[0] == 0
    pr = _state(world)["prs"][0]
    assert (pr["state"], pr["merge_method"], pr["admin"]) == ("MERGED", "rebase", False)
    assert _run(capsys, "pr", "merge", "2", "--squash")[0] == 1
    assert _state(world)["prs"][1]["state"] == "OPEN"
    assert _run(capsys, "pr", "merge", "2", "--squash", "--admin")[0] == 0
    pr = _state(world)["prs"][1]
    assert (pr["merge_method"], pr["admin"]) == ("squash", True)


def test_closing_keyword_only_on_default_branch(world, capsys):
    world(
        {
            "issues": {
                "5": {"title": "t", "body": "", "state": "open"},
                "6": {"title": "t", "body": "", "state": "open"},
            },
            "prs": [],
        }
    )
    _run(capsys, "pr", "create", "--head", "f", "--title", "x", "--body", "Closes #5")
    _run(capsys, "pr", "create", "--base", "feat", "--head", "g", "--title", "fixes #6")
    _run(capsys, "pr", "merge", "101", "--squash")
    _run(capsys, "pr", "merge", "102", "--squash")
    issues = _state(world)["issues"]
    assert issues["5"]["state"] == "closed"
    assert issues["5"]["closed_by"] == {"kind": "pr", "pr": 101}
    assert issues["6"]["state"] == "open"

    query = "query { repository { issue(number: 5) { timelineItems(itemTypes: CLOSED_EVENT) "
    query += "{ nodes { ... on ClosedEvent { closer { __typename } } } } } } }"
    _, out, _ = _run(capsys, "api", "graphql", "-f", f"query={query}")
    nodes = json.loads(out)["data"]["repository"]["issue"]["timelineItems"]["nodes"]
    assert nodes == [{"closer": {"__typename": "PullRequest", "number": 101}}]
    _run(capsys, "issue", "close", "6")
    _, out, _ = _run(capsys, "api", "graphql", "-f", f"query={query}", "-F", "number=6")
    nodes = json.loads(out)["data"]["repository"]["issue"]["timelineItems"]["nodes"]
    assert nodes == [{"closer": None}]


def test_fault_injection_consumed_by_times(world, capsys):
    world(
        {
            "issues": {"1": {"title": "t", "body": "", "state": "open"}},
            "faults": {"issue close": {"code": 1, "stderr": "boom", "times": 1}},
        }
    )
    code, _, err = _run(capsys, "issue", "close", "1")
    assert code == 1 and "boom" in err
    assert _state(world)["issues"]["1"]["state"] == "open"
    assert _run(capsys, "issue", "close", "1")[0] == 0
    assert _state(world)["issues"]["1"]["state"] == "closed"
    assert len(_state(world)["calls"]) == 2


def test_api_method_fault_key(world, capsys):
    world({"issues": {}, "faults": {"api PATCH": {"code": 2, "stderr": "nope", "times": 1}}})
    code, _, err = _run(capsys, "api", "-X", "PATCH", "repos/o/r/issues/comments/1")
    assert code == 2 and "nope" in err


def test_body_limit_65536(world, capsys):
    world({"issues": {"1": {"title": "t", "body": "", "state": "open"}}})
    big = "x" * 65537
    code, _, err = _run(capsys, "issue", "comment", "1", "--body", big)
    assert code == 1 and "65536" in err
    code, _, err = _run(capsys, "issue", "create", "--title", "t", "--body", big)
    assert code == 1 and "65536" in err
    code, _, err = _run(capsys, "pr", "create", "--head", "h", "--title", "t", "--body", big)
    assert code == 1 and "65536" in err
    assert _run(capsys, "issue", "comment", "1", "--body", "x" * 65536)[0] == 0
