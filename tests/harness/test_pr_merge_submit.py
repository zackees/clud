"""Exercise the bundled merge submitter through a scripted Claude session."""

from __future__ import annotations

import pytest

from tests.harness.harness import Harness

HEAD = "a" * 40


@pytest.mark.parametrize(
    ("queue", "draft", "queued", "head", "review", "fault", "expected", "merged", "enqueued"),
    [
        (True, False, False, HEAD, False, False, "enqueued", False, True),
        (False, False, False, HEAD, False, False, "submitted", True, False),
        (True, False, True, HEAD, False, False, "already queued", False, True),
        (True, False, False, "b" * 40, False, False, "head changed", False, False),
        (True, True, False, HEAD, False, False, "open and ready", False, False),
        (False, False, False, HEAD, True, False, "review required", False, False),
        (True, False, False, HEAD, True, False, "GitHub returned errors", False, False),
        (True, False, False, HEAD, False, True, "simulated failure", False, False),
    ],
)
def test_agent_submits_exact_head(
    harness: Harness, queue: bool, draft: bool, queued: bool, head: str,
    review: bool, fault: bool, expected: str, merged: bool, enqueued: bool,
) -> None:
    harness.write_gh_state({
        "repo": "o/r", "issues": {}, "queue_branches": ["main"] if queue else [],
        "requires_review": review, "fail": fault,
        "prs": [{"number": 101, "state": "OPEN", "head_oid": head,
                 "draft": draft, "queued": queued}],
    })
    command = f"clud tool run github/pr_merge.py -- 101 --repo o/r --match-head-commit {HEAD}"
    script = {"default_text": "DONE", "roles": [{"name": "main", "steps": [
        {"tool_use": {"name": "Bash", "input": {"command": command}}},
        {"text": "DONE"},
    ]}]}
    result = harness.run("Submit the specified pull request.", script)
    assert result.returncode == 0, result.stderr
    results = [item for request in result.requests for item in request.get("tool_results", [])]
    matching = [item for item in results if expected in str(item.get("content", ""))]
    assert matching, results
    assert matching[0]["is_error"] is (not merged and not enqueued)
    state = harness.read_gh_state()
    pr = state["prs"][0]
    assert (pr["state"] == "MERGED") is merged
    assert pr["queued"] is enqueued
    calls = state["calls"]
    mutations = [call for call in calls if any("enqueuePullRequest" in arg for arg in call)]
    merges = [call for call in calls if call[:2] == ["pr", "merge"]]
    assert len(mutations) == (1 if queue and not draft and not queued and head == HEAD
                              and not fault else 0)
    assert len(merges) == int(merged or (review and not queue))
    if mutations:
        assert f"sha={HEAD}" in mutations[0]
    if merges:
        assert "--match-head-commit" in merges[0]
        assert HEAD in merges[0]
        assert "--squash" in merges[0]
        assert "--admin" not in merges[0]


def test_agent_enqueues_only_the_requested_repository(harness: Harness) -> None:
    def repository() -> dict:
        return {"issues": {}, "queue_branches": ["main"], "prs": [
            {"number": 101, "state": "OPEN", "head_oid": HEAD, "queued": False},
        ]}

    harness.write_gh_state({
        "repo": "o/primary", "repositories": {
            "o/primary": repository(), "o/sister": repository(),
        },
    })
    command = f"clud tool run github/pr_merge.py -- 101 --repo o/sister --match-head-commit {HEAD}"
    script = {"default_text": "DONE", "roles": [{"name": "main", "steps": [
        {"tool_use": {"name": "Bash", "input": {"command": command}}},
        {"text": "DONE"},
    ]}]}
    result = harness.run("Submit the specified pull request.", script)
    assert result.returncode == 0, result.stderr
    results = [item for request in result.requests for item in request.get("tool_results", [])]
    assert any("enqueued" in str(item.get("content", "")) for item in results), results
    repositories = harness.read_gh_state()["repositories"]
    assert repositories["o/sister"]["prs"][0]["queued"] is True
    assert repositories["o/primary"]["prs"][0]["queued"] is False
