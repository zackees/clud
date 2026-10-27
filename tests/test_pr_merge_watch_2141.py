"""Explicit merge completion waits (#2141)."""

from __future__ import annotations

import importlib.util
import sys
from types import SimpleNamespace

import pytest

from tests.test_pr_merge_watch_2035 import SCRIPT, STARTED, _follow, _queue


@pytest.fixture
def watcher(monkeypatch):
    name = "clud_test_pr_merge_watch_2141"
    spec = importlib.util.spec_from_file_location(name, SCRIPT)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    monkeypatch.setattr(module, "_utc_now", lambda: STARTED)
    monkeypatch.setattr(module.time, "sleep", lambda _s: None)
    module.FOLLOW = (0, module.time.monotonic() + 60)
    try:
        yield module
    finally:
        sys.modules.pop(name, None)


@pytest.fixture
def merge_watcher(watcher, monkeypatch):
    monkeypatch.setattr(watcher, "EXPLICIT_MERGE_WAIT", True, raising=False)
    return watcher


def test_until_merged_is_explicit_and_incompatible_with_no_merge_wait(watcher):
    assert watcher.parse_args(["42", "--until", "merged"]).until == "merged"
    with pytest.raises(SystemExit):
        watcher.parse_args(["42", "--until", "merged", "--no-merge-wait"])


def test_until_merged_follows_directly_without_running_check_watch(watcher, monkeypatch):
    followed = []
    log = object()
    monkeypatch.setattr(watcher.WatchLog, "create", lambda *_: log)
    monkeypatch.setattr(watcher, "install_kill_handlers", lambda: None)
    monkeypatch.setattr(watcher, "watch", lambda *_a, **_kw: pytest.fail("must follow directly"))

    def follow(pr, repo, observed_log):
        followed.append((pr, repo, observed_log))
        raise SystemExit(watcher.EXIT_GREEN)

    monkeypatch.setattr(watcher, "follow_merge", follow)
    with pytest.raises(SystemExit) as result:
        watcher.main(["42", "--repo", "o/r", "--until", "merged"])
    assert result.value.code == watcher.EXIT_GREEN
    assert followed == [(42, "o/r", log)]


def test_until_merged_timeout_is_not_a_merge(merge_watcher, capsys):
    merge_watcher.FOLLOW = (0, merge_watcher.time.monotonic() - 1)
    with pytest.raises(SystemExit) as result:
        merge_watcher.follow_merge(42, "o/r", None)
    assert result.value.code == merge_watcher.EXIT_TIMEOUT
    assert "TIMEOUT  #42" in capsys.readouterr().out


def test_follow_reports_position_and_state_changes(merge_watcher, monkeypatch, capsys):
    watcher = merge_watcher
    first = _queue(watcher, entry="AWAITING_CHECKS")
    first.position = 6
    moved = _queue(watcher, entry="AWAITING_CHECKS")
    moved.position = 2
    ready = _queue(watcher, entry="MERGEABLE")
    ready.position = 2
    states = [first, first, moved, ready, _queue(watcher, state="MERGED", entry=None)]
    assert _follow(watcher, monkeypatch, states) == watcher.EXIT_GREEN
    out = capsys.readouterr().out
    assert out.count("position=6 state=AWAITING_CHECKS") == 1
    assert out.count("position=2 state=AWAITING_CHECKS") == 1
    assert out.count("position=2 state=MERGEABLE") == 1


def test_persistent_unmergeable_has_distinct_verdict(merge_watcher, monkeypatch):
    watcher = merge_watcher
    states = [_queue(watcher, entry="UNMERGEABLE"), _queue(watcher, entry="UNMERGEABLE")]
    assert _follow(watcher, monkeypatch, states) == watcher.EXIT_UNMERGEABLE


def test_transient_unmergeable_does_not_end_the_wait(merge_watcher, monkeypatch):
    watcher = merge_watcher
    states = [
        _queue(watcher, entry="UNMERGEABLE"), _queue(watcher, entry="AWAITING_CHECKS"),
        _queue(watcher, entry="UNMERGEABLE"), _queue(watcher, state="MERGED", entry=None),
    ]
    assert _follow(watcher, monkeypatch, states) == watcher.EXIT_GREEN


def test_dequeue_is_ejection_even_when_auto_merge_remains_enabled(merge_watcher, monkeypatch):
    watcher = merge_watcher
    states = [_queue(watcher), _queue(watcher, entry=None, auto=True)]
    assert _follow(watcher, monkeypatch, states) == watcher.EXIT_EJECTED


def test_partial_graphql_result_is_not_queue_removal(merge_watcher):
    data = {"data": {"repository": {"pullRequest": {
        "state": "OPEN", "baseRefName": "main", "autoMergeRequest": None,
        "mergeQueueEntry": None, "timelineItems": {"nodes": []},
    }}}, "errors": [{"message": "queue unavailable"}]}
    assert merge_watcher.parse_queue_state(data) is None
    data.pop("errors")
    del data["data"]["repository"]["pullRequest"]["mergeQueueEntry"]
    assert merge_watcher.parse_queue_state(data) is None


def test_unreadable_queue_sample_does_not_imply_ejection(merge_watcher, monkeypatch):
    watcher = merge_watcher
    states = [_queue(watcher), None, _queue(watcher, state="MERGED", entry=None)]
    assert _follow(watcher, monkeypatch, states) == watcher.EXIT_GREEN


@pytest.mark.parametrize(("entry", "nodes"), [
    ({"state": "QUEUED", "position": 1, "headCommit": 123}, []),
    (None, [{"createdAt": "not-a-date", "reason": "MANUAL"}]),
])
def test_malformed_nested_queue_response_is_unreadable(merge_watcher, entry, nodes):
    data = {"data": {"repository": {"pullRequest": {
        "state": "OPEN", "baseRefName": "main", "autoMergeRequest": None,
        "mergeQueueEntry": entry, "timelineItems": {"nodes": nodes},
    }}}}
    assert merge_watcher.parse_queue_state(data) is None


def test_github_call_timeout_is_bounded_by_remaining_deadline(merge_watcher, monkeypatch):
    watcher = merge_watcher
    watcher.FOLLOW = (20, 10.25)
    monkeypatch.setattr(watcher.time, "monotonic", lambda: 10.0)
    timeouts = []

    def run(*_args, **kwargs):
        timeouts.append(kwargs["timeout"])
        return SimpleNamespace(returncode=0, stdout="{}", stderr="")

    monkeypatch.setattr(watcher.RunningProcess, "run", run)
    assert watcher.gh("api", "graphql").ok
    assert timeouts == [0.25]


def test_failed_group_does_not_imply_a_queued_pr_was_removed(merge_watcher, monkeypatch):
    watcher = merge_watcher
    jobs = [({"name": "CI"}, [{"name": "build", "conclusion": "failure"}])]
    states = [_queue(watcher), _queue(watcher, state="MERGED", entry=None)]
    assert _follow(watcher, monkeypatch, states, jobs) == watcher.EXIT_GREEN


def test_interval_is_bounded_by_deadline(merge_watcher, monkeypatch):
    watcher = merge_watcher
    clock = [0.0]
    sleeps = []
    monkeypatch.setattr(watcher.time, "monotonic", lambda: clock[0])

    def sleep(seconds):
        sleeps.append(seconds)
        clock[0] += seconds

    monkeypatch.setattr(watcher.time, "sleep", sleep)
    monkeypatch.setattr(watcher, "fetch_queue_state", lambda *_: _queue(watcher))
    monkeypatch.setattr(watcher, "fetch_merge_group_jobs", lambda *_: [])
    monkeypatch.setattr(watcher, "fetch_required_check_names", lambda *_: set())
    watcher.FOLLOW = (20, 1.0)
    with pytest.raises(SystemExit) as result:
        watcher.follow_merge(42, "o/r", None)
    assert result.value.code == watcher.EXIT_TIMEOUT
    assert sleeps == [1.0]
