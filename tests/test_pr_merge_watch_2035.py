"""#2035: the watcher follows a queued PR to its merge, or to its ejection.

A green watch on a merge-queue PR exited 0 while the PR was still OPEN in the
queue, and the hook refused every other way to wait for the merge. A PR in a
merge queue or set to auto-merge is now followed until GitHub reports it
MERGED (0), the queue ejects it (13), or its auto-merge is withdrawn (12).
"""

from __future__ import annotations

import importlib.util
import sys
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "github" / "pr_merge_watch.py"
STARTED = datetime(2026, 10, 22, 12, 0, tzinfo=UTC)


@pytest.fixture
def watcher(monkeypatch):
    name = "clud_test_pr_merge_watch_2035"
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


def _queue(watcher, state="OPEN", entry="QUEUED", *, auto=False, reason=None, removed=None):
    return watcher.QueueState(
        state=state, base_ref="master", auto_merge=auto,
        entry_state=entry, position=1 if entry else None,
        queue_sha="mq1" if entry else None,
        removal_reason=reason, removed_at=removed,
    )


def _follow(watcher, monkeypatch, states, jobs=None, required=None) -> int:
    seq = iter(states)
    monkeypatch.setattr(watcher, "fetch_queue_state", lambda _r, _p: next(seq))
    monkeypatch.setattr(watcher, "fetch_merge_group_jobs", lambda _r, _s: jobs or [])
    monkeypatch.setattr(watcher, "fetch_required_check_names", lambda _r, _b: required)
    with pytest.raises(SystemExit) as exit_info:
        watcher.follow_merge(1847, "obs-rust/obs-studio", None)
    return exit_info.value.code


def test_a_queued_pr_exits_zero_only_once_merged(watcher, monkeypatch, capsys) -> None:
    states = [_queue(watcher), _queue(watcher), _queue(watcher, state="MERGED", entry=None)]
    assert _follow(watcher, monkeypatch, states) == watcher.EXIT_GREEN
    out = capsys.readouterr().out
    assert "ENQUEUED  #1847" in out
    assert "MERGED  #1847" in out


def test_a_required_merge_group_failure_ejects_at_once_and_names_the_job(
    watcher, monkeypatch, capsys
) -> None:
    jobs = [({"name": "CI"}, [
        {"name": "lint", "conclusion": None},
        {"name": "advisory", "conclusion": "failure"},
        {"name": "build", "conclusion": "failure", "html_url": "https://x/job/9"},
    ])]
    # Only one state: the watch must not poll again for the rest of the matrix.
    code = _follow(watcher, monkeypatch, [_queue(watcher)], jobs, required={"build", "lint"})
    assert code == watcher.EXIT_EJECTED
    out = capsys.readouterr().out
    assert "CI / build: failure https://x/job/9" in out
    assert "advisory" not in out


def test_a_cancelled_merge_group_job_is_a_queue_rebuild_not_a_verdict(watcher, monkeypatch) -> None:
    jobs = [({"name": "CI"}, [{"name": "build", "conclusion": "cancelled"}])]
    states = [_queue(watcher), _queue(watcher, state="MERGED", entry=None)]
    assert _follow(watcher, monkeypatch, states, jobs) == watcher.EXIT_GREEN


def test_removal_from_the_queue_names_the_reason(watcher, monkeypatch, capsys) -> None:
    removed = _queue(watcher, entry=None, reason="FAILED_CHECKS", removed=STARTED + timedelta(seconds=5))
    assert _follow(watcher, monkeypatch, [_queue(watcher), removed]) == watcher.EXIT_EJECTED
    assert "FAILED_CHECKS" in capsys.readouterr().out


def test_an_old_removal_does_not_eject_a_pr_that_is_still_auto_merging(watcher, monkeypatch) -> None:
    old = STARTED - timedelta(days=1)
    states = [
        _queue(watcher, entry=None, auto=True, reason="MANUAL", removed=old),
        _queue(watcher),
        _queue(watcher, state="MERGED", entry=None),
    ]
    assert _follow(watcher, monkeypatch, states) == watcher.EXIT_GREEN


def test_withdrawn_auto_merge_says_the_pr_is_not_enqueued(watcher, monkeypatch, capsys) -> None:
    states = [_queue(watcher, entry=None, auto=True)] + [_queue(watcher, entry=None)] * 3
    assert _follow(watcher, monkeypatch, states) == watcher.EXIT_NOT_ENQUEUED
    out = capsys.readouterr().out
    assert "NOT-ENQUEUED  #1847" in out
    assert "GREEN" not in out


def test_a_closed_pr_is_not_merged(watcher, monkeypatch) -> None:
    assert _follow(watcher, monkeypatch, [_queue(watcher, state="CLOSED", entry=None)]) == (
        watcher.EXIT_PR_CLOSED
    )


def test_the_graphql_answer_parses_into_queue_state(watcher) -> None:
    data = {"data": {"repository": {"pullRequest": {
        "state": "OPEN", "baseRefName": "master", "autoMergeRequest": None,
        "mergeQueueEntry": {"state": "AWAITING_CHECKS", "position": 2, "headCommit": {"oid": "abc"}},
        "timelineItems": {"nodes": [{"reason": "MANUAL", "createdAt": "2026-10-22T12:00:05Z"}]},
    }}}}
    q = watcher.parse_queue_state(data)
    assert (q.entry_state, q.position, q.queue_sha, q.auto_merge) == ("AWAITING_CHECKS", 2, "abc", False)
    assert q.removal_reason == "MANUAL"
    assert q.removed_at == STARTED + timedelta(seconds=5)
    assert watcher.parse_queue_state({"errors": []}) is None


def test_no_merge_wait_stops_at_green_and_reports_the_pending_merge(
    watcher, monkeypatch, capsys
) -> None:
    assert watcher.parse_args(["1847"]).merge_wait is True
    assert watcher.parse_args(["1847", "--no-merge-wait"]).merge_wait is False
    monkeypatch.setattr(watcher, "fetch_queue_state", lambda _r, _p: _queue(watcher))
    monkeypatch.setattr(watcher, "follow_merge", lambda *_a: pytest.fail("must not follow"))
    monkeypatch.setattr(watcher, "MERGE_WAIT", False)
    opts = watcher.CancelOptions(
        on=set(), mode="none", timeout=1, require=False, dry_run=False,
        ignore_permission_errors=True, no_retry=True,
    )
    with pytest.raises(SystemExit) as exit_info:
        watcher._exit_green(1847, "o/r", "sha", opts, None, [], {})
    assert exit_info.value.code == watcher.EXIT_GREEN
    assert "NOT-MERGED-YET  #1847" in capsys.readouterr().out


def test_green_on_a_queued_pr_follows_the_merge(watcher, monkeypatch, capsys) -> None:
    monkeypatch.setattr(watcher, "fetch_queue_state", lambda _r, _p: _queue(watcher))
    followed = []

    def follow(pr, repo, _log):
        followed.append((pr, repo))
        raise SystemExit(watcher.EXIT_GREEN)

    monkeypatch.setattr(watcher, "follow_merge", follow)
    opts = watcher.CancelOptions(
        on=set(), mode="none", timeout=1, require=False, dry_run=False,
        ignore_permission_errors=True, no_retry=True,
    )
    with pytest.raises(SystemExit):
        watcher._exit_green(1847, "o/r", "sha", opts, None, [], {})
    assert followed == [(1847, "o/r")]
    assert "FOLLOWING  #1847" in capsys.readouterr().out
