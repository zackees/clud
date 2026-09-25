"""Focused unit tests for the bundled meta-issue checker (issue #1404)."""

from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "github" / "is_meta_issue.py"


@pytest.fixture
def meta():
    name = "clud_test_is_meta_issue"
    spec = importlib.util.spec_from_file_location(name, SCRIPT)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    try:
        yield module
    finally:
        sys.modules.pop(name, None)


def fake_gh(meta, *, sub_issues="[]", body="", fail_sub=False, calls=None):
    def _gh(*args, **_kwargs):
        if calls is not None:
            calls.append(args)
        if args[:2] == ("repo", "view"):
            return meta.GhResult(0, json.dumps({"nameWithOwner": "zackees/clud"}), "")
        if args[0] == "api" and args[1].endswith("/sub_issues"):
            if fail_sub:
                return meta.GhResult(1, "", "HTTP 404: Not Found")
            return meta.GhResult(0, sub_issues, "")
        if args[0] == "api":
            return meta.GhResult(0, json.dumps({"number": 7, "body": body}), "")
        raise AssertionError(f"unexpected gh call: {args}")

    return _gh


def run(meta, capsys, argv):
    code = meta.main(argv)
    out, err = capsys.readouterr()
    return code, out, err


def test_native_sub_issues_only(meta, monkeypatch, capsys):
    subs = json.dumps(
        [
            {"number": 11, "state": "open", "title": "a"},
            {"number": 12, "state": "closed", "title": "b"},
        ]
    )
    monkeypatch.setattr(meta, "gh", fake_gh(meta, sub_issues=subs, body="plain body"))
    code, out, _ = run(meta, capsys, ["7", "--repo", "zackees/clud"])
    assert code == 0
    data = json.loads(out)
    assert data["meta"] is True
    assert data["sub_issues"] == [
        {"number": 11, "state": "open"},
        {"number": 12, "state": "closed"},
    ]
    assert data["task_list_refs"] == []


def test_task_list_only(meta, monkeypatch, capsys):
    body = "\n".join(
        [
            "Intro mentioning #99 outside a task list.",
            "- [ ] #101 first",
            "- [x] https://github.com/zackees/clud/issues/102",
            "* [X] other/repo#5 cross-repo",
            "  - [ ] follow-up https://github.com/zackees/clud/pull/103",
            "- plain bullet #200",
            "- [ ] #101 duplicate",
        ]
    )
    monkeypatch.setattr(meta, "gh", fake_gh(meta, body=body))
    code, out, _ = run(meta, capsys, ["7", "--repo", "zackees/clud"])
    assert code == 0
    data = json.loads(out)
    assert data["meta"] is True
    assert data["sub_issues"] == []
    assert data["task_list_refs"] == [101, 102, "other/repo#5", 103]


def test_parse_task_list_refs_is_pure(meta):
    assert meta.parse_task_list_refs("- [ ] #3\n- note #4\n- [x] #3") == [3]
    assert meta.parse_task_list_refs(None) == []


def test_neither_is_not_meta(meta, monkeypatch, capsys):
    monkeypatch.setattr(meta, "gh", fake_gh(meta, body="- just a bullet #9"))
    code, out, _ = run(meta, capsys, ["7", "--repo", "zackees/clud"])
    assert code == 0
    assert json.loads(out) == {"meta": False, "sub_issues": [], "task_list_refs": []}


def test_gh_failure_on_sub_issues_exits_2(meta, monkeypatch, capsys):
    monkeypatch.setattr(meta, "gh", fake_gh(meta, fail_sub=True))
    code, out, err = run(meta, capsys, ["7", "--repo", "zackees/clud"])
    assert code == 2
    assert "error" in err
    assert "HTTP 404" in err
    assert '"meta"' not in out
    assert out.strip() == ""


def test_unparseable_sub_issues_exits_2(meta, monkeypatch, capsys):
    monkeypatch.setattr(meta, "gh", fake_gh(meta, sub_issues="not json"))
    code, out, err = run(meta, capsys, ["7", "--repo", "zackees/clud"])
    assert code == 2
    assert "error" in err
    assert out.strip() == ""


def test_url_argument_supplies_repo_and_number(meta, monkeypatch, capsys):
    assert meta.parse_issue_arg("https://github.com/octo/demo/issues/42", None) == (
        "octo/demo",
        42,
    )
    calls: list = []
    monkeypatch.setattr(meta, "gh", fake_gh(meta, calls=calls))
    code, _, _ = run(meta, capsys, ["https://github.com/octo/demo/issues/42"])
    assert code == 0
    assert ("api", "repos/octo/demo/issues/42/sub_issues", "--paginate") in calls
    assert ("api", "repos/octo/demo/issues/42") in calls
    assert not any(c[:2] == ("repo", "view") for c in calls)


def test_repo_resolved_from_gh_when_absent(meta, monkeypatch, capsys):
    calls: list = []
    monkeypatch.setattr(meta, "gh", fake_gh(meta, calls=calls))
    code, _, _ = run(meta, capsys, ["7"])
    assert code == 0
    assert calls[0][:2] == ("repo", "view")
    assert ("api", "repos/zackees/clud/issues/7") in calls


def test_usage_error_exits_1(meta, capsys):
    code, out, err = run(meta, capsys, ["not-an-issue"])
    assert code == 1
    assert out == ""
    assert "usage" in err
