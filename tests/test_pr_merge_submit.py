"""Queue-aware PR submission (#2235)."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "crates/clud-bin/assets/tools/github/pr_merge.py"
SHA = "1" * 40


@pytest.fixture
def submitter():
    spec = importlib.util.spec_from_file_location("clud_test_pr_merge_submit", SCRIPT)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    try:
        yield module
    finally:
        sys.modules.pop(spec.name, None)


def fake_gh(
    monkeypatch, submitter, *, queue=True, head=SHA, state="OPEN", draft=False, queued=False
):
    calls = []

    def run(*args):
        calls.append(args)
        if "graphql" in args and any(
            "query=" in arg and "enqueuePullRequest" in arg for arg in args
        ):
            return {"data": {"enqueuePullRequest": {"mergeQueueEntry": {"id": "entry"}}}}
        if any("mergeQueue(branch:" in arg for arg in args):
            return {"data": {"repository": {"mergeQueue": {"id": "queue"} if queue else None}}}
        if "graphql" in args:
            return {"data": {"repository": {"pullRequest": {
                "id": "PR_node", "headRefOid": head, "baseRefName": "release/x",
                "state": state, "isDraft": draft,
                "mergeQueueEntry": {"id": "existing"} if queued else None,
            }}}}
        return {}

    monkeypatch.setattr(submitter, "gh_json", run)
    monkeypatch.setattr(submitter, "gh_command", run)
    return calls


def test_queue_submits_exact_head_without_auto_merge(submitter, monkeypatch):
    calls = fake_gh(monkeypatch, submitter)
    assert submitter.submit("talos-hq/clud", 2226, SHA) == "enqueued"
    assert any("expectedHeadOid" in " ".join(args) and f"sha={SHA}" in args for args in calls)
    assert any("base=release/x" in args for args in calls)
    assert not any("merge" in args and "pr" in args for args in calls)


def test_ordinary_branch_uses_normal_squash_with_expected_head(submitter, monkeypatch):
    calls = fake_gh(monkeypatch, submitter, queue=False)
    assert submitter.submit("talos-hq/clud", 42, SHA) == "submitted"
    assert calls[-1] == (
        "pr", "merge", "42", "--repo", "github.com/talos-hq/clud", "--squash",
        "--match-head-commit", SHA,
    )


def test_matching_already_queued_does_not_mutate(submitter, monkeypatch):
    calls = fake_gh(monkeypatch, submitter, queued=True)
    assert submitter.submit("talos-hq/clud", 2226, SHA) == "already queued"
    assert len(calls) == 1


def test_missing_pr_queue_entry_fails_without_mutation(submitter, monkeypatch):
    calls = fake_gh(monkeypatch, submitter)
    original = submitter.gh_json

    def absent(*args):
        payload = original(*args)
        if "pullRequest" in payload.get("data", {}).get("repository", {}):
            payload["data"]["repository"]["pullRequest"].pop("mergeQueueEntry")
        return payload

    monkeypatch.setattr(submitter, "gh_json", absent)
    with pytest.raises(submitter.SubmitError, match="queue entry"):
        submitter.submit("talos-hq/clud", 2226, SHA)
    assert len(calls) == 1


@pytest.mark.parametrize("kwargs", [{"head": "2" * 40}, {"state": "CLOSED"}, {"draft": True}])
def test_unacceptable_pr_never_mutates(submitter, monkeypatch, kwargs):
    calls = fake_gh(monkeypatch, submitter, **kwargs)
    with pytest.raises(submitter.SubmitError):
        submitter.submit("talos-hq/clud", 2226, SHA)
    assert len(calls) == 1


def test_failed_queue_lookup_never_falls_through_to_merge(submitter, monkeypatch):
    calls = fake_gh(monkeypatch, submitter)
    original = submitter.gh_json

    def fail_rules(*args):
        if any("mergeQueue(branch:" in arg for arg in args):
            raise submitter.SubmitError("queue unavailable")
        return original(*args)

    monkeypatch.setattr(submitter, "gh_json", fail_rules)
    with pytest.raises(submitter.SubmitError, match="queue unavailable"):
        submitter.submit("talos-hq/clud", 2226, SHA)
    assert len(calls) == 1


@pytest.mark.parametrize("queue", [[], {"id": None}, {"id": ""}, "queue"])
def test_malformed_queue_fails_closed(submitter, monkeypatch, queue):
    calls = fake_gh(monkeypatch, submitter)
    original = submitter.gh_json

    def invalid_queue(*args):
        if any("mergeQueue(branch:" in arg for arg in args):
            return {"data": {"repository": {"mergeQueue": queue}}}
        return original(*args)

    monkeypatch.setattr(submitter, "gh_json", invalid_queue)
    with pytest.raises(submitter.SubmitError, match="invalid branch queue state"):
        submitter.submit("talos-hq/clud", 2226, SHA)
    assert len(calls) == 1


def test_empty_queue_mutation_result_is_never_reported_as_success(submitter, monkeypatch):
    calls = fake_gh(monkeypatch, submitter)
    original = submitter.gh_json

    def no_result(*args):
        if any("enqueuePullRequest" in arg for arg in args):
            calls.append(args)
            return {"data": {"enqueuePullRequest": {"mergeQueueEntry": None}}}
        return original(*args)

    monkeypatch.setattr(submitter, "gh_json", no_result)
    with pytest.raises(submitter.SubmitError, match="inspect the PR"):
        submitter.submit("talos-hq/clud", 2226, SHA)
    assert len(calls) == 3


@pytest.mark.parametrize(("repo", "number", "sha"), [
    ("github.com/a/b", 1, SHA), ("a/b", 0, SHA), ("a/b", 1, "123"),
])
def test_invalid_input_never_queries_github(submitter, monkeypatch, repo, number, sha):
    calls = fake_gh(monkeypatch, submitter)
    with pytest.raises(submitter.SubmitError):
        submitter.submit(repo, number, sha)
    assert calls == []


def test_graphql_partial_errors_never_return_success(submitter, monkeypatch):
    monkeypatch.setattr(
        submitter, "gh_command", lambda *_: '{"data": {}, "errors": [{"message": "denied"}]}'
    )
    with pytest.raises(submitter.SubmitError, match="GitHub returned errors"):
        submitter.gh_json("api", "graphql")


def test_missing_or_invalid_queue_field_never_falls_through(submitter, monkeypatch):
    calls = fake_gh(monkeypatch, submitter)
    original = submitter.gh_json

    def absent(*args):
        if any("mergeQueue(branch:" in arg for arg in args):
            return {"data": {"repository": {}}}
        return original(*args)

    monkeypatch.setattr(submitter, "gh_json", absent)
    with pytest.raises(submitter.SubmitError, match="invalid branch queue state"):
        submitter.submit("talos-hq/clud", 2226, SHA)
    assert len(calls) == 1
