"""Focused unit tests for the bundled PR merge watcher (issue #528)."""

from __future__ import annotations

import importlib.util
import json
import sys
from datetime import UTC, datetime
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "github" / "pr_merge_watch.py"


@pytest.fixture
def watcher():
    name = "clud_test_pr_merge_watch"
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


def gate_snapshot(
    watcher,
    checks: list,
    *,
    state: str = "OPEN",
    mergeable: str = "MERGEABLE",
    probe=None,
    coderabbit=None,
    human_review_ids: frozenset[int] = frozenset(),
):
    return watcher.GateSnapshot(
        pr=watcher.PRSnapshot(527, state, mergeable, "abc123", "main"),
        checks=checks,
        human_review_ids=human_review_ids,
        coderabbit_probe=probe or watcher.CodeRabbitProbe("not_detected", 0),
        coderabbit=coderabbit or watcher.CodeRabbitObservation("quiet"),
    )


def test_watch_log_announces_path_and_uses_start_then_relative_time(
    watcher, tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    moments = iter([100.0, 100.01])
    monkeypatch.setattr(watcher.time, "monotonic", lambda: next(moments))
    monkeypatch.setattr(
        watcher,
        "_utc_now",
        lambda: datetime(2026, 7, 11, 23, 41, 2, 123000, tzinfo=UTC),
    )

    log = watcher.WatchLog.create(527, "zackees/clud", root=tmp_path)
    announced = capsys.readouterr().err.strip()
    assert announced == "LOG .clud/logs/pr-merge-watch/20260711T234102.123Z-pr-527.jsonl"

    log.emit(
        "checks",
        checks={"total": 2, "pending": 2, "failed": 0, "succeeded": 0, "skipped": 0},
    )
    log.close()

    records = [json.loads(line) for line in log.path.read_text(encoding="utf-8").splitlines()]
    assert records == [
        {
            "v": 1,
            "ts": "2026-07-11T23:41:02.123Z",
            "elapsed_sec": 0.0,
            "event": "START",
            "repo": "zackees/clud",
            "pr": 527,
            "log_path": ".clud/logs/pr-merge-watch/20260711T234102.123Z-pr-527.jsonl",
        },
        {
            "v": 1,
            "elapsed_sec": 0.01,
            "event": "checks",
            "checks": {"total": 2, "pending": 2, "failed": 0, "succeeded": 0, "skipped": 0},
        },
    ]


def test_main_announces_log_before_entering_watcher(
    watcher, tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(watcher, "_watch_root", lambda: tmp_path)

    def fake_watch(*_args, **_kwargs) -> int:
        assert capsys.readouterr().err.startswith("LOG .clud/logs/pr-merge-watch/")
        return watcher.EXIT_GREEN

    monkeypatch.setattr(watcher, "watch", fake_watch)

    assert watcher.main(["527", "--repo", "zackees/clud"]) == watcher.EXIT_GREEN


def test_check_counts_reconcile_all_buckets(watcher) -> None:
    checks = [
        watcher.CheckRow("linux", "pass", "SUCCESS"),
        watcher.CheckRow("windows", "fail", "FAILURE"),
        watcher.CheckRow("mac", "pending", "IN_PROGRESS"),
        watcher.CheckRow("docs", "skipping", "SKIPPED"),
        watcher.CheckRow("cancelled", "cancel", "CANCELLED"),
    ]

    assert watcher.check_counts(checks) == {
        "total": 5,
        "pending": 1,
        "failed": 2,
        "succeeded": 1,
        "skipped": 1,
    }


def test_required_checks_include_context_and_app_bound_entries(
    watcher, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(
        watcher,
        "gh_json",
        lambda *args: {
            "contexts": ["legacy"],
            "checks": [{"context": "linux", "app_id": 123}, {"context": "mac"}],
        },
    )
    assert watcher.fetch_required_check_names("zackees/clud", "main") == {
        "legacy",
        "linux",
        "mac",
    }


def test_fetch_checks_distinguishes_no_checks_from_api_failure(
    watcher, monkeypatch: pytest.MonkeyPatch
) -> None:
    responses = iter(
        [
            watcher.GhResult(1, "", "no checks reported on the branch"),
            watcher.GhResult(1, "", "HTTP 502"),
            watcher.GhResult(0, "not-json", ""),
        ]
    )
    monkeypatch.setattr(watcher, "gh", lambda *args: next(responses))

    assert watcher.fetch_checks(527, "zackees/clud") == []
    assert watcher.fetch_checks(527, "zackees/clud") is None
    assert watcher.fetch_checks(527, "zackees/clud") is None


@pytest.mark.parametrize(
    ("comments", "expected_state", "expected_reason"),
    [
        (
            [
                {
                    "user": {"login": "coderabbitai[bot]"},
                    "body": "Review skipped because review credits have been exhausted.",
                }
            ],
            "skipped",
            "credits_exhausted",
        ),
        (
            [
                {
                    "user": {"login": "coderabbitai[bot]"},
                    "body": "Review skipped. Auto reviews are disabled for this branch.",
                }
            ],
            "skipped",
            "review_skipped",
        ),
        ([], "quiet", None),
    ],
)
def test_coderabbit_status_comments_are_advisory(
    watcher, comments: list[dict], expected_state: str, expected_reason: str | None
) -> None:
    observation = watcher.classify_coderabbit([], comments)
    assert observation.state == expected_state
    assert observation.reason == expected_reason
    assert not observation.actionable


def test_coderabbit_unresolved_thread_is_actionable(watcher) -> None:
    threads = [
        {
            "isResolved": False,
            "comments": {
                "nodes": [
                    {
                        "databaseId": 91,
                        "body": "Handle the error before continuing.",
                        "author": {"login": "coderabbitai[bot]"},
                    }
                ]
            },
        }
    ]

    observation = watcher.classify_coderabbit(threads, [])
    assert observation.state == "actionable"
    assert observation.actionable
    assert observation.unresolved_threads == 1


def test_coderabbit_unresolved_thread_wins_over_skipped_status(watcher) -> None:
    threads = [
        {
            "isResolved": False,
            "comments": {
                "nodes": [
                    {
                        "databaseId": 91,
                        "body": "This remains actionable.",
                        "author": {"login": "coderabbitai[bot]"},
                    }
                ]
            },
        }
    ]
    comments = [
        {
            "user": {"login": "coderabbitai[bot]"},
            "body": "Review skipped because credits are exhausted.",
        }
    ]

    observation = watcher.classify_coderabbit(threads, comments)
    assert observation.state == "actionable"
    assert observation.actionable


def test_coderabbit_credit_status_without_review_skipped_is_neutral(watcher) -> None:
    observation = watcher.classify_coderabbit(
        [],
        [
            {
                "user": {"login": "coderabbitai[bot]"},
                "body": "Automated reviews are paused: your review quota has been exhausted.",
            }
        ],
    )
    assert observation.state == "skipped"
    assert observation.reason == "credits_exhausted"
    assert not observation.actionable


def test_existing_coderabbit_thread_interrupts_first_review_poll(
    watcher, monkeypatch: pytest.MonkeyPatch
) -> None:
    observation = watcher.CodeRabbitObservation(
        "actionable",
        actionable=True,
        unresolved_threads=1,
        ids=frozenset({91}),
    )
    monkeypatch.setattr(watcher, "gh_json", lambda *args: [])
    monkeypatch.setattr(watcher, "fetch_coderabbit", lambda *args: observation)

    state = watcher.ReviewState(coderabbit_enabled=True)
    assert state.update(527, "zackees/clud")


def test_coderabbit_api_failure_is_degraded_and_non_actionable(
    watcher, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(watcher, "gh_json", lambda *args: None)
    observation = watcher.fetch_coderabbit("zackees/clud", 527)
    assert observation.state == "degraded"
    assert not observation.actionable


def test_coderabbit_transient_api_failure_does_not_disable_later_observation(
    watcher, monkeypatch: pytest.MonkeyPatch
) -> None:
    observations = iter(
        [
            watcher.CodeRabbitObservation("degraded", reason="api_error"),
            watcher.CodeRabbitObservation(
                "actionable", actionable=True, unresolved_threads=1, ids=frozenset({91})
            ),
        ]
    )
    monkeypatch.setattr(watcher, "gh_json", lambda *args: [])
    monkeypatch.setattr(watcher, "fetch_coderabbit", lambda *args: next(observations))
    state = watcher.ReviewState(coderabbit_enabled=True)

    assert not state.update(527, "zackees/clud")
    assert state.coderabbit_enabled
    assert state.update(527, "zackees/clud")


def test_coderabbit_presence_probe_handles_absence_and_api_degradation(
    watcher, monkeypatch: pytest.MonkeyPatch
) -> None:
    responses = iter(
        [
            [{"number": 11}, {"number": 10}],
            [],
            [],
        ]
    )
    monkeypatch.setattr(watcher, "gh_json", lambda *args: next(responses))
    probe = watcher.probe_coderabbit("zackees/clud")
    assert probe.state == "not_detected"
    assert probe.sampled_merged_prs == 2

    monkeypatch.setattr(watcher, "gh_json", lambda *args: None)
    degraded = watcher.probe_coderabbit("zackees/clud")
    assert degraded.state == "degraded"
    assert degraded.sampled_merged_prs == 0


def test_coderabbit_presence_probe_handles_zero_and_detects_on_fifth_pr(
    watcher, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(watcher, "gh_json", lambda *args: [])
    absent = watcher.probe_coderabbit("zackees/clud")
    assert absent == watcher.CodeRabbitProbe("not_detected", 0)

    responses = iter(
        [
            [{"number": 5}, {"number": 4}, {"number": 3}, {"number": 2}, {"number": 1}],
            [],
            [],
            [],
            [],
            [{"user": {"login": "coderabbitai[bot]"}}],
        ]
    )
    monkeypatch.setattr(watcher, "gh_json", lambda *args: next(responses))
    detected = watcher.probe_coderabbit("zackees/clud")
    assert detected == watcher.CodeRabbitProbe("detected", 5)


def test_coderabbit_not_detected_state_makes_no_bot_api_call(
    watcher, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(watcher, "gh_json", lambda *args: [])
    monkeypatch.setattr(
        watcher,
        "fetch_coderabbit",
        lambda *args: pytest.fail("CodeRabbit API should be disabled when not detected"),
    )
    state = watcher.ReviewState(coderabbit_enabled=False)
    assert not state.update(527, "zackees/clud")


def test_required_red_logs_then_cancels_without_another_poll(
    watcher, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    log = watcher.WatchLog.create(527, "zackees/clud", root=tmp_path)
    monkeypatch.setattr(
        watcher.PRSnapshot,
        "fetch",
        lambda *args: watcher.PRSnapshot(527, "OPEN", "UNKNOWN", "abc123", "main"),
    )
    monkeypatch.setattr(watcher, "fetch_required_check_names", lambda *args: {"linux"})
    polls = 0

    def gates(*_args, **_kwargs):
        nonlocal polls
        polls += 1
        return gate_snapshot(
            watcher,
            [
                watcher.CheckRow("linux", "fail", "FAILURE"),
                watcher.CheckRow("windows", "pending", "IN_PROGRESS"),
            ],
            mergeable="UNKNOWN",
        )

    monkeypatch.setattr(watcher, "fetch_gate_snapshot", gates)
    monkeypatch.setattr(watcher, "emit_progress_report", lambda *args: None)
    monkeypatch.setattr(
        watcher,
        "_build_failure_report",
        lambda *args: watcher.FailureReport(args[0], None, "boom", "test failure"),
    )
    cancellations: list[str] = []
    monkeypatch.setattr(
        watcher,
        "cancel_pr_runs",
        lambda _pr, _repo, sha, _opts, _log=None: cancellations.append(sha) or 1,
    )
    opts = watcher.CancelOptions(
        on={"fail"},
        mode="runs",
        timeout=30,
        require=False,
        dry_run=False,
        ignore_permission_errors=True,
        no_retry=False,
    )

    with pytest.raises(SystemExit) as exc:
        watcher.watch(527, "zackees/clud", 60, 3600, None, opts, log)

    assert exc.value.code == watcher.EXIT_REQUIRED_FAIL
    assert polls == 1
    assert cancellations == ["abc123"]
    records = [json.loads(line) for line in log.path.read_text(encoding="utf-8").splitlines()]
    events = [record["event"] for record in records]
    assert events.index("required_failure") < events.index("cancel") < events.index("EXIT")


def test_empty_required_set_fails_fast_instead_of_waiting_for_the_matrix(
    watcher, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Protection configured with zero contexts must mean all-checks-required.

    The old fallback treated an empty required set as "nothing is required",
    so a red fast lane was advisory and the watcher idled until every slow
    matrix lane (the Mac builds) finished. An empty set is protection data
    that names no checks — fail fast, don't wait it out.
    """
    log = watcher.WatchLog.create(527, "zackees/clud", root=tmp_path)
    monkeypatch.setattr(
        watcher.PRSnapshot,
        "fetch",
        lambda *args: watcher.PRSnapshot(527, "OPEN", "UNKNOWN", "abc123", "main"),
    )
    monkeypatch.setattr(watcher, "fetch_required_check_names", lambda *args: set())
    polls = 0

    def gates(*_args, **_kwargs):
        nonlocal polls
        polls += 1
        return gate_snapshot(
            watcher,
            [
                watcher.CheckRow("linux lint", "fail", "FAILURE"),
                watcher.CheckRow("macos arm64", "pending", "IN_PROGRESS"),
            ],
            mergeable="UNKNOWN",
        )

    monkeypatch.setattr(watcher, "fetch_gate_snapshot", gates)
    monkeypatch.setattr(watcher, "emit_progress_report", lambda *args: None)
    monkeypatch.setattr(
        watcher,
        "_build_failure_report",
        lambda *args: watcher.FailureReport(args[0], None, "boom", "test failure"),
    )
    monkeypatch.setattr(
        watcher,
        "cancel_pr_runs",
        lambda _pr, _repo, _sha, _opts, _log=None: 1,
    )
    opts = watcher.CancelOptions(
        on={"fail"},
        mode="runs",
        timeout=30,
        require=False,
        dry_run=False,
        ignore_permission_errors=True,
        no_retry=False,
    )

    with pytest.raises(SystemExit) as exc:
        watcher.watch(527, "zackees/clud", 20, 3600, None, opts, log)

    assert exc.value.code == watcher.EXIT_REQUIRED_FAIL
    assert polls == 1


def test_empty_rollup_on_a_fresh_push_is_not_green(
    watcher, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A fresh push registers no checks for the first few seconds; an empty
    rollup must read as "no data yet" and keep polling, never as GREEN."""
    log = watcher.WatchLog.create(527, "zackees/clud", root=tmp_path)
    monkeypatch.setattr(
        watcher.PRSnapshot,
        "fetch",
        lambda *args: watcher.PRSnapshot(527, "OPEN", "UNKNOWN", "abc123", "main"),
    )
    monkeypatch.setattr(watcher, "fetch_required_check_names", lambda *args: None)
    monkeypatch.setattr(watcher, "_sleep_remaining_interval", lambda *args: None)
    monkeypatch.setattr(watcher, "emit_progress_report", lambda *args: None)
    polls = 0

    def gates(*_args, **_kwargs):
        nonlocal polls
        polls += 1
        if polls == 1:
            return gate_snapshot(watcher, [])
        return gate_snapshot(
            watcher, [watcher.CheckRow("linux", "pass", "SUCCESS")], mergeable="MERGEABLE"
        )

    monkeypatch.setattr(watcher, "fetch_gate_snapshot", gates)
    opts = watcher.CancelOptions(
        on={"fail"},
        mode="runs",
        timeout=30,
        require=False,
        dry_run=False,
        ignore_permission_errors=True,
        no_retry=False,
    )

    with pytest.raises(SystemExit) as exc:
        watcher.watch(527, "zackees/clud", 20, 3600, None, opts, log)

    assert exc.value.code == watcher.EXIT_GREEN
    assert polls == 2


def test_default_poll_interval_is_quick(watcher) -> None:
    ns = watcher.parse_args(["527"])
    assert ns.interval == 20


def test_required_red_diagnoses_before_cancelling(
    watcher, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    log = watcher.WatchLog.create(527, "zackees/clud", root=tmp_path)
    monkeypatch.setattr(
        watcher.PRSnapshot,
        "fetch",
        lambda *args: watcher.PRSnapshot(527, "OPEN", "UNKNOWN", "abc123", "main"),
    )
    monkeypatch.setattr(watcher, "fetch_required_check_names", lambda *args: {"linux"})
    monkeypatch.setattr(
        watcher,
        "fetch_gate_snapshot",
        lambda *args, **kwargs: gate_snapshot(
            watcher,
            [watcher.CheckRow("linux", "fail", "FAILURE")],
            mergeable="UNKNOWN",
        ),
    )
    monkeypatch.setattr(watcher, "emit_progress_report", lambda *args: None)
    order: list[str] = []
    monkeypatch.setattr(
        watcher,
        "cancel_pr_runs",
        lambda *_args, **_kwargs: order.append("cancel") or 1,
    )
    monkeypatch.setattr(
        watcher,
        "_build_failure_report",
        lambda check, _repo: (
            order.append("diagnose") or watcher.FailureReport(check, None, "boom", "test failure")
        ),
    )
    opts = watcher.CancelOptions({"fail"}, "runs", 30, False, False, True, False)

    with pytest.raises(SystemExit) as exc:
        watcher.watch(527, "zackees/clud", 60, 3600, None, opts, log)

    assert exc.value.code == watcher.EXIT_REQUIRED_FAIL
    # Diagnose first. Cancelling raced the failing job's log becoming
    # readable, so the caller got a bare "FAIL <name>" with nothing to act
    # on — the opposite of what failing fast is for. The probe is one
    # bounded request, so the matrix minutes it costs are seconds.
    assert order == ["diagnose", "cancel"]


def test_check_api_failure_cannot_synthesize_green(
    watcher, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    log = watcher.WatchLog.create(527, "zackees/clud", root=tmp_path)
    monkeypatch.setattr(
        watcher.PRSnapshot,
        "fetch",
        lambda *args: watcher.PRSnapshot(527, "OPEN", "MERGEABLE", "abc123", "main"),
    )
    monkeypatch.setattr(watcher, "fetch_required_check_names", lambda *args: {"linux"})
    gates = iter(
        [
            None,
            gate_snapshot(watcher, [watcher.CheckRow("linux", "pass", "SUCCESS")]),
        ]
    )
    monkeypatch.setattr(watcher, "fetch_gate_snapshot", lambda *args, **kwargs: next(gates))
    monkeypatch.setattr(watcher, "emit_progress_report", lambda *args: None)
    sleeps: list[int] = []
    monkeypatch.setattr(watcher.time, "sleep", lambda seconds: sleeps.append(seconds))
    opts = watcher.CancelOptions(set(), "runs", 30, False, False, True, False)

    with pytest.raises(SystemExit) as exc:
        watcher.watch(527, "zackees/clud", 60, 3600, None, opts, log)

    assert exc.value.code == watcher.EXIT_GREEN
    assert sleeps == [pytest.approx(60, abs=0.1)]
    records = [json.loads(line) for line in log.path.read_text(encoding="utf-8").splitlines()]
    assert any(
        record.get("event") == "api_degraded" and record.get("source") == "gate_snapshot"
        for record in records
    )


def test_slow_advisory_work_is_bounded_by_poll_interval(
    watcher, monkeypatch: pytest.MonkeyPatch
) -> None:
    clock = [0.0]
    monkeypatch.setattr(watcher.time, "monotonic", lambda: clock[0])
    sleeps: list[float] = []

    def sleep(seconds: float) -> None:
        sleeps.append(seconds)
        clock[0] += seconds

    monkeypatch.setattr(watcher.time, "sleep", sleep)
    monkeypatch.setattr(
        watcher.PRSnapshot,
        "fetch",
        lambda *args: watcher.PRSnapshot(527, "OPEN", "MERGEABLE", "abc123", "main"),
    )
    monkeypatch.setattr(watcher, "fetch_required_check_names", lambda *args: {"linux"})
    gates = iter(
        [
            gate_snapshot(watcher, [watcher.CheckRow("linux", "pending", "IN_PROGRESS")]),
            gate_snapshot(watcher, [watcher.CheckRow("linux", "pass", "SUCCESS")], probe=None),
        ]
    )
    monkeypatch.setattr(watcher, "emit_progress_report", lambda *args: None)
    include_flags: list[bool] = []

    def slow_gate(*_args, include_coderabbit: bool, **_kwargs):
        include_flags.append(include_coderabbit)
        if len(include_flags) == 1:
            clock[0] += 12
        return next(gates)

    monkeypatch.setattr(watcher, "fetch_gate_snapshot", slow_gate)
    opts = watcher.CancelOptions(set(), "runs", 30, False, False, True, False)

    with pytest.raises(SystemExit) as exc:
        watcher.watch(527, "zackees/clud", 60, 3600, None, opts)

    assert exc.value.code == watcher.EXIT_GREEN
    assert include_flags == [True, False]
    assert sleeps == [48]


def test_advisory_failure_does_not_terminate_viable_required_checks(
    watcher, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    log = watcher.WatchLog.create(527, "zackees/clud", root=tmp_path)
    monkeypatch.setattr(
        watcher.PRSnapshot,
        "fetch",
        lambda *args: watcher.PRSnapshot(527, "OPEN", "MERGEABLE", "abc123", "main"),
    )
    monkeypatch.setattr(watcher, "fetch_required_check_names", lambda *args: {"linux"})
    monkeypatch.setattr(
        watcher,
        "fetch_gate_snapshot",
        lambda *args, **kwargs: gate_snapshot(
            watcher,
            [
                watcher.CheckRow("linux", "pass", "SUCCESS"),
                watcher.CheckRow("optional", "fail", "FAILURE"),
            ],
        ),
    )
    monkeypatch.setattr(watcher, "emit_progress_report", lambda *args: None)
    monkeypatch.setattr(
        watcher,
        "cancel_pr_runs",
        lambda *args: pytest.fail("advisory failure must not cancel runs"),
    )
    opts = watcher.CancelOptions(set(), "runs", 30, False, False, True, False)

    with pytest.raises(SystemExit) as exc:
        watcher.watch(527, "zackees/clud", 60, 3600, None, opts, log)

    assert exc.value.code == watcher.EXIT_GREEN


@pytest.mark.parametrize("probe_state", ["degraded", "not_detected"])
def test_green_ci_with_actionable_coderabbit_feedback_exits_two(
    watcher, tmp_path: Path, monkeypatch: pytest.MonkeyPatch, probe_state: str
) -> None:
    log = watcher.WatchLog.create(527, "zackees/clud", root=tmp_path)
    monkeypatch.setattr(
        watcher.PRSnapshot,
        "fetch",
        lambda *args: watcher.PRSnapshot(527, "OPEN", "MERGEABLE", "abc123", "main"),
    )
    monkeypatch.setattr(watcher, "fetch_required_check_names", lambda *args: {"linux"})
    monkeypatch.setattr(
        watcher,
        "fetch_gate_snapshot",
        lambda *args, **kwargs: gate_snapshot(
            watcher,
            [watcher.CheckRow("linux", "pass", "SUCCESS")],
            probe=watcher.CodeRabbitProbe(probe_state, 0),
            coderabbit=watcher.CodeRabbitObservation(
                "actionable", actionable=True, unresolved_threads=1, ids=frozenset({91})
            ),
        ),
    )
    opts = watcher.CancelOptions(set(), "runs", 30, False, False, True, False)

    with pytest.raises(SystemExit) as exc:
        watcher.watch(527, "zackees/clud", 60, 3600, None, opts, log)

    assert exc.value.code == watcher.EXIT_REVIEW_ACTIVITY


def test_combined_gate_snapshot_parses_checks_reviews_and_coderabbit(
    watcher, monkeypatch: pytest.MonkeyPatch
) -> None:
    payload = {
        "data": {
            "repository": {
                "pullRequest": {
                    "number": 527,
                    "state": "OPEN",
                    "mergeable": "MERGEABLE",
                    "headRefOid": "abc123",
                    "baseRefName": "main",
                    "reviews": {
                        "nodes": [
                            {
                                "databaseId": 7,
                                "state": "COMMENTED",
                                "author": {"login": "human"},
                            }
                        ],
                        "pageInfo": {"hasNextPage": False},
                    },
                    "reviewThreads": {
                        "nodes": [
                            {
                                "isResolved": False,
                                "comments": {
                                    "nodes": [
                                        {
                                            "databaseId": 91,
                                            "body": "Fix this.",
                                            "author": {"login": "coderabbitai[bot]"},
                                        }
                                    ],
                                    "pageInfo": {"hasNextPage": False},
                                },
                            }
                        ],
                        "pageInfo": {"hasNextPage": False},
                    },
                    "comments": {
                        "nodes": [],
                        "pageInfo": {"hasPreviousPage": False},
                    },
                    "commits": {
                        "nodes": [
                            {
                                "commit": {
                                    "statusCheckRollup": {
                                        "contexts": {
                                            "nodes": [
                                                {
                                                    "__typename": "CheckRun",
                                                    "name": "linux",
                                                    "status": "COMPLETED",
                                                    "conclusion": "SUCCESS",
                                                    "detailsUrl": "https://example/check",
                                                },
                                                {
                                                    "__typename": "StatusContext",
                                                    "context": "legacy",
                                                    "state": "PENDING",
                                                    "targetUrl": None,
                                                },
                                            ],
                                            "pageInfo": {"hasNextPage": False},
                                        }
                                    }
                                }
                            }
                        ]
                    },
                },
                "recent": {
                    "nodes": [
                        {
                            "number": 526,
                            "reviews": {
                                "nodes": [{"author": {"login": "coderabbitai[bot]"}}],
                                "pageInfo": {"hasNextPage": False},
                            },
                        }
                    ]
                },
            }
        }
    }
    monkeypatch.setattr(watcher, "gh_json", lambda *args: payload)

    gate = watcher.fetch_gate_snapshot("zackees/clud", 527, include_coderabbit=True)

    assert gate is not None
    assert [(check.name, check.bucket) for check in gate.checks] == [
        ("linux", "pass"),
        ("legacy", "pending"),
    ]
    assert gate.human_review_ids == frozenset({7})
    assert gate.coderabbit_probe == watcher.CodeRabbitProbe("detected", 1)
    assert gate.coderabbit is not None
    assert gate.coderabbit.actionable

    contexts = payload["data"]["repository"]["pullRequest"]["commits"]["nodes"][0][
        "commit"
    ]["statusCheckRollup"]["contexts"]
    contexts["pageInfo"]["hasNextPage"] = True
    assert watcher.fetch_gate_snapshot("zackees/clud", 527, include_coderabbit=True) is None

    contexts["pageInfo"]["hasNextPage"] = False
    payload["data"]["repository"]["pullRequest"]["reviewThreads"]["pageInfo"][
        "hasNextPage"
    ] = True
    assert watcher.fetch_gate_snapshot("zackees/clud", 527, include_coderabbit=True) is None


def test_initially_merged_pr_is_success(
    watcher, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    log = watcher.WatchLog.create(527, "zackees/clud", root=tmp_path)
    monkeypatch.setattr(
        watcher.PRSnapshot,
        "fetch",
        lambda *args: watcher.PRSnapshot(527, "MERGED", "UNKNOWN", "abc123", "main"),
    )
    opts = watcher.CancelOptions(set(), "runs", 30, False, False, True, False)

    with pytest.raises(SystemExit) as exc:
        watcher.watch(527, "zackees/clud", 60, 3600, None, opts, log)

    assert exc.value.code == watcher.EXIT_GREEN


def test_cancellation_never_targets_a_different_head_sha(
    watcher, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    log = watcher.WatchLog.create(527, "zackees/clud", root=tmp_path)
    monkeypatch.setattr(
        watcher,
        "gh_json",
        lambda *args: {
            "workflow_runs": [
                {"id": 101, "status": "in_progress", "head_sha": "abc123"},
                {"id": 202, "status": "queued", "head_sha": "different"},
                {"id": 303, "status": "queued"},
                {"id": 404, "status": "queued", "head_sha": None},
                {"id": 505, "status": "queued", "head_sha": ""},
                {"id": 606, "status": "queued", "head_sha": 123},
            ]
        },
    )
    cancelled: list[int] = []

    def fake_gh(*args, **_kwargs):
        cancelled.append(int(args[-1].split("/")[-2]))
        return watcher.GhResult(0, "", "")

    monkeypatch.setattr(watcher, "gh", fake_gh)
    opts = watcher.CancelOptions({"fail"}, "runs", 30, False, False, True, False)

    assert watcher.cancel_pr_runs(527, "zackees/clud", "abc123", opts, log) == 1
    assert cancelled == [101]


def test_cancellation_permission_error_does_not_replace_original_exit(
    watcher, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    log = watcher.WatchLog.create(527, "zackees/clud", root=tmp_path)
    monkeypatch.setattr(
        watcher,
        "gh_json",
        lambda *args: {
            "workflow_runs": [{"id": 123, "status": "in_progress", "head_sha": "abc123"}]
        },
    )
    monkeypatch.setattr(
        watcher,
        "gh",
        lambda *args, **kwargs: watcher.GhResult(1, "", "HTTP 403: Resource not accessible"),
    )
    opts = watcher.CancelOptions(
        on={"review"},
        mode="runs",
        timeout=30,
        require=True,
        dry_run=False,
        ignore_permission_errors=False,
        no_retry=False,
    )

    with pytest.raises(SystemExit) as exc:
        watcher._exit_after_cancel(
            watcher.EXIT_REVIEW_ACTIVITY,
            "review",
            527,
            "zackees/clud",
            "abc123",
            opts,
            log,
        )

    assert exc.value.code == watcher.EXIT_REVIEW_ACTIVITY


# A verbatim slice of a real failing Windows integration job, kept with its
# runner prefixes intact. Everything below asserts against the shape GitHub
# actually emits, because the shape is what the old patterns got wrong.
REAL_PYTEST_LOG = (
    "2026-09-03T19:45:30.9660131Z tests/integration/test_mock_agents.py"
    "::TestLoopMode::test_codex_loop_iterations FAILED [ 79%]\n"
    "2026-09-03T19:47:21.6282237Z E               TimeoutError: process timed out\n"
    "2026-09-03T19:47:21.6447671Z =========================== short test summary info"
    " ===========================\n"
    "2026-09-03T19:47:21.6460585Z FAILED tests/integration/test_mock_agents.py"
    "::TestLoopMode::test_codex_loop_iterations - AssertionError: timed out after 30s\n"
)

GH_RUN_VIEW_LOG = (
    "Test windows-x64\tRun integration suite\t2026-09-03T19:47:21.6460585Z "
    "FAILED tests/integration/test_mock_agents.py::TestLoopMode::test_codex_loop_iterations\n"
)


def test_normalize_log_strips_both_runner_prefix_shapes(watcher) -> None:
    """The REST job endpoint and `gh run view` prefix lines differently."""
    rest = watcher.normalize_log("2026-09-03T19:47:21.6460585Z FAILED tests/x.py::t")
    run_view = watcher.normalize_log(GH_RUN_VIEW_LOG)
    assert rest == "FAILED tests/x.py::t"
    assert run_view.startswith("FAILED tests/integration/test_mock_agents.py")


def test_a_pytest_failure_is_classified_as_a_test_failure_not_a_network_blip(
    watcher,
) -> None:
    """Regression: the anchored patterns could never match a prefixed line.

    A pytest failure fell through every earlier pattern to the `timed out`
    one and was reported to the caller as "network/transient" — which reads
    as "retry it", the wrong call for a real red.
    """
    sample = watcher.normalize_log(REAL_PYTEST_LOG)
    label = next((lbl for pat, lbl in watcher.CLASSIFIERS if pat.search(sample)), None)
    assert label == "test failure"


def test_the_first_error_names_the_failing_test(watcher) -> None:
    captured: dict[str, object] = {}

    def fake_gh(*args, **kwargs):
        captured["args"] = args
        return watcher.GhResult(0, REAL_PYTEST_LOG, "")

    watcher.gh = fake_gh
    first_err, label = watcher.classify_failure("zackees/clud", "999", "12345")
    assert "test_codex_loop_iterations" in first_err
    assert label == "test failure"


def test_the_probe_reads_the_job_not_the_run(watcher, monkeypatch) -> None:
    """`gh run view --log-failed` refuses while the run is in progress.

    On the fail-fast path the run is always in progress, so a run-level probe
    returns nothing exactly when the caller needs it. The job endpoint serves
    a finished job's log regardless of its siblings.
    """
    calls: list[tuple] = []

    def fake_gh(*args, **kwargs):
        calls.append(args)
        if args[0] == "api":
            return watcher.GhResult(0, REAL_PYTEST_LOG, "")
        return watcher.GhResult(1, "", "run is still in progress")

    monkeypatch.setattr(watcher, "gh", fake_gh)
    text = watcher.fetch_failure_log("zackees/clud", "999", "12345")
    assert "test_codex_loop_iterations" in text
    assert calls == [
        ("api", "repos/zackees/clud/actions/jobs/12345/logs", "--allow-escape-sequences")
    ]


def test_a_failing_checks_link_yields_both_ids(watcher) -> None:
    link = "https://github.com/zackees/clud/actions/runs/33798803736/job/100794241127"
    assert watcher._extract_run_id_from_link(link) == "33798803736"
    assert watcher._extract_job_id_from_link(link) == "100794241127"


def test_a_run_only_link_still_yields_the_run(watcher) -> None:
    link = "https://github.com/zackees/clud/actions/runs/33798803736"
    assert watcher._extract_run_id_from_link(link) == "33798803736"
    assert watcher._extract_job_id_from_link(link) is None


def test_a_timed_out_probe_is_a_failed_call_not_a_crash(watcher, monkeypatch) -> None:
    """The probe runs ahead of cancellation; it must never be what blocks it."""

    def boom(*args, **kwargs):
        raise TimeoutError("probe hung")

    monkeypatch.setattr(watcher.RunningProcess, "run", boom)
    res = watcher.gh("api", "whatever", timeout=1.0)
    assert not res.ok
    assert "timed out" in res.stderr


def test_running_process_own_timeout_is_a_failed_call_too(watcher, monkeypatch) -> None:
    """What running-process actually raises is `TimeoutExpired`, which is not
    a `TimeoutError`; the builtin-only catch let a hung probe escape (#1175)."""

    def boom(*args, **kwargs):
        raise watcher.TimeoutExpired(["gh", "api"], 1.0)

    monkeypatch.setattr(watcher.RunningProcess, "run", boom)
    res = watcher.gh("api", "whatever", timeout=1.0)
    assert not res.ok
    assert "timed out" in res.stderr


# Verbatim from the Windows unit job of run 33807762372, prefixes intact. The
# harness aborted rather than reporting a failed test, so the ONLY line that
# explains the red is GitHub's step annotation at the very end.
REAL_ABORTED_HARNESS_LOG = (
    "2026-09-03T21:30:48.3499037Z test tools::tests::"
    "oversized_codes_are_truncated_not_panicked_on ... ok\n"
    "2026-09-03T21:30:56.0850669Z test fixture_ids::"
    "module_path_is_crate_rooted_in_this_target ... ok\n"
    "2026-09-03T21:35:09.5122615Z ##[error]failing Rust harnesses: "
    "reaper-b0f613a8f3a49815.exe (rc=-1073740791)\n"
    "2026-09-03T21:35:09.6533894Z ##[error]Process completed with exit code 1.\n"
)


def test_a_passing_test_named_after_a_panic_is_not_the_first_error(watcher) -> None:
    """Regression: the probe reported a PASSING test as the failure.

    `oversized_codes_are_truncated_not_panicked_on ... ok` contains the
    substring "panicked", and the unanchored search matched it. A first-error
    line that names the wrong thing is worse than none: it sends the reader
    somewhere real and irrelevant.
    """
    sample = watcher.normalize_log(REAL_ABORTED_HARNESS_LOG)
    first = watcher.first_error_line(sample)
    assert "oversized_codes" not in first
    assert "reaper-b0f613a8f3a49815.exe" in first
    assert "rc=-1073740791" in first


def test_a_step_annotation_is_recognised_as_the_error(watcher) -> None:
    """When a harness aborts, `##[error]` is the whole explanation."""
    sample = watcher.normalize_log(REAL_ABORTED_HARNESS_LOG)
    first = watcher.first_error_line(sample)
    assert not first.startswith("##[error]"), "the annotation prefix should be stripped"
    assert first.startswith("failing Rust harnesses:")
    label = next((lbl for pat, lbl in watcher.CLASSIFIERS if pat.search(sample)), None)
    assert label == "test failure"


def test_a_run_with_no_error_line_yields_empty_not_a_false_positive(watcher) -> None:
    passing = (
        "2026-09-03T21:30:48.3499037Z test a::b_panicked_at_startup ... ok\n"
        "2026-09-03T21:30:48.3499037Z test result: ok. 2 passed; 0 failed\n"
    )
    assert watcher.first_error_line(watcher.normalize_log(passing)) == ""


# -- #1175: gh() streams are never None, and a failed cancel is reported -----


def _opts(**overrides):
    base = dict(
        on={"fail"},
        mode="runs",
        timeout=30,
        require=False,
        dry_run=False,
        ignore_permission_errors=True,
        no_retry=False,
    )
    base.update(overrides)
    return base


def test_gh_asks_for_stderr_separately_and_never_returns_none_streams(
    watcher, monkeypatch
) -> None:
    """running-process merges stderr into stdout unless `stderr=PIPE` is
    passed; without it every `GhResult.stderr` was None and the first failed
    cancel crashed on `"HTTP 403" in None` (#1175)."""
    seen: dict[str, object] = {}

    class Done:
        returncode = 1
        stdout = None
        stderr = None

    def fake_run(argv, **kwargs):
        seen["argv"] = argv
        seen["kwargs"] = kwargs
        return Done()

    monkeypatch.setattr(watcher.RunningProcess, "run", fake_run)
    res = watcher.gh("api", "-X", "POST", "repos/o/r/actions/runs/1/cancel")
    assert seen["argv"][0] == "gh"
    assert seen["kwargs"]["stderr"] is watcher.PIPE
    assert res.stdout == ""
    assert res.stderr == ""
    assert not res.ok


@pytest.mark.parametrize(
    ("stderr", "status"),
    [
        ("HTTP 403: Resource not accessible by integration", "permission_denied"),
        ("gh: HTTP 422: Cannot cancel a workflow run that is completed", "already_completed"),
        ("", "error"),
    ],
)
def test_a_failed_cancel_is_classified_and_never_raises(
    watcher, capsys, stderr: str, status: str
) -> None:
    watcher._report_cancel(
        123,
        watcher.GhResult(1, "", stderr),
        watcher.CancelOptions(**_opts()),
        None,
        "runs",
    )
    out = capsys.readouterr().out
    assert f"CANCEL  id=123 status={status}" in out


# -- #1330: judge checks by newest run per workflow ---------------------------
#
# Every case below is driven by recorded REST shapes under
# tests/fixtures/pr_merge_watch/ (`commits/{sha}/check-runs` items and
# `actions/runs?head_sha=` items). No test makes a live gh call.

FIXTURES = ROOT / "tests" / "fixtures" / "pr_merge_watch"
CI_YML = ".github/workflows/ci.yml"


def load_case(family: str, case_id: str) -> dict:
    return json.loads((FIXTURES / f"{family}.json").read_text(encoding="utf-8"))[case_id]


def judge(watcher, case: dict, **overrides):
    kwargs = {"statuses": case.get("statuses"), "head_branch": case.get("head_branch")}
    kwargs.update(overrides)
    return watcher.judge_check_runs(
        case["check_runs"],
        case["workflow_runs"],
        case["head_sha"],
        set(case["required"]),
        **kwargs,
    )


def by_name(verdict, name: str) -> list:
    return [j for j in verdict.judgments if j.name == name]


def head_gate(watcher, poll: dict, statuses: list | None = None):
    return watcher.GateSnapshot(
        pr=watcher.PRSnapshot(527, "OPEN", "MERGEABLE", poll["head_sha"], "main"),
        checks=[watcher.CheckRow("rollup", "pending", "IN_PROGRESS")],
        human_review_ids=frozenset(),
        coderabbit_probe=watcher.CodeRabbitProbe("not_detected", 0),
        coderabbit=watcher.CodeRabbitObservation("quiet"),
        head_checks=watcher.HeadChecks(
            poll["check_runs"], poll["workflow_runs"], list(statuses or poll.get("statuses") or [])
        ),
    )


def run_watch(watcher, tmp_path, monkeypatch, polls: list[dict], required, *, reread=None):
    """Drive `watch()` over recorded polls; return (exit code, polls, cancel calls)."""
    log = watcher.WatchLog.create(527, "zackees/clud", root=tmp_path)
    first = polls[0]["head_sha"]
    rereads = iter(reread or [])
    fetches = {"n": 0}

    def fetch(*_args):
        # The first fetch is watch()'s initial snapshot; later ones are the
        # head re-reads before acting on a cancellation-derived verdict.
        fetches["n"] += 1
        head = first if fetches["n"] == 1 else (next(rereads, None) or first)
        return watcher.PRSnapshot(527, "OPEN", "MERGEABLE", head, "main")

    monkeypatch.setattr(watcher.PRSnapshot, "fetch", fetch)
    monkeypatch.setattr(watcher, "fetch_required_check_names", lambda *args: set(required))
    monkeypatch.setattr(watcher, "_sleep_remaining_interval", lambda *args: None)
    monkeypatch.setattr(watcher, "emit_progress_report", lambda *args: None)
    monkeypatch.setattr(
        watcher,
        "_build_failure_report",
        lambda check, _repo: watcher.FailureReport(check, None, "", None),
    )
    seen = {"polls": 0}

    def gates(*_args, **_kwargs):
        index = min(seen["polls"], len(polls) - 1)
        seen["polls"] += 1
        return head_gate(watcher, polls[index])

    monkeypatch.setattr(watcher, "fetch_gate_snapshot", gates)
    cancels: list[dict] = []
    monkeypatch.setattr(
        watcher,
        "cancel_pr_runs",
        lambda *args, **kwargs: cancels.append({"args": args, "kwargs": kwargs}) or 1,
    )
    opts = watcher.CancelOptions({"fail"}, "runs", 30, False, False, True, False)
    with pytest.raises(SystemExit) as exc:
        watcher.watch(527, "zackees/clud", 20, 3600, None, opts, log)
    return exc.value.code, seen["polls"], cancels


def scoped_cancel(watcher, monkeypatch, case: dict, scope: dict[str, int]) -> list[int]:
    monkeypatch.setattr(
        watcher, "gh_json", lambda *args: {"workflow_runs": case["workflow_runs"]}
    )
    cancelled: list[int] = []

    def fake_gh(*args, **_kwargs):
        cancelled.append(int(args[-1].split("/")[-2]))
        return watcher.GhResult(0, "", "")

    monkeypatch.setattr(watcher, "gh", fake_gh)
    opts = watcher.CancelOptions({"fail"}, "runs", 30, False, False, True, False)
    watcher.cancel_pr_runs(527, "zackees/clud", case["head_sha"], opts, None, scope=scope)
    return cancelled


# ---- exit codes ----


def test_new_exit_codes_are_distinct_and_documented(watcher) -> None:
    assert watcher.EXIT_APPROVAL_REQUIRED == 5
    assert watcher.EXIT_NEVER_REPORTED == 6
    assert watcher.EXIT_STALE == 7
    codes = [
        watcher.EXIT_GREEN,
        watcher.EXIT_REQUIRED_FAIL,
        watcher.EXIT_REVIEW_ACTIVITY,
        watcher.EXIT_PR_CLOSED,
        watcher.EXIT_TIMEOUT,
        watcher.EXIT_APPROVAL_REQUIRED,
        watcher.EXIT_NEVER_REPORTED,
        watcher.EXIT_STALE,
    ]
    assert len(set(codes)) == len(codes)
    doc = watcher.__doc__
    for code in ("5  approval required", "6  never reported", "7  stale"):
        assert code in doc
    assert "Supersession rule" in doc


def test_help_states_the_supersession_rule(watcher, capsys) -> None:
    with pytest.raises(SystemExit):
        watcher.parse_args(["--help"])
    out = capsys.readouterr().out
    assert "supersession rule" in out
    assert "7 stale" in out


# ---- #1329 regression ----


def test_regression_1329_cancelled_then_superseded_static_checks_pass(
    watcher, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    case = json.loads((FIXTURES / "regression_1329.json").read_text(encoding="utf-8"))
    verdict = judge(watcher, case)
    assert verdict.state == "pass"
    assert verdict.failing == []

    code, _polls, cancels = run_watch(watcher, tmp_path, monkeypatch, [case], case["required"])
    assert code == watcher.EXIT_GREEN
    assert cancels == []


# ---- C: concurrency and supersession ----


def test_c1_cancelled_run_waits_on_its_in_progress_replacement(watcher) -> None:
    verdict = judge(watcher, load_case("concurrency", "C1"))
    assert verdict.state == "pending"
    assert verdict.failing == []
    assert verdict.failing_run_ids == {}


def test_c2_replacement_green_passes(watcher, tmp_path, monkeypatch) -> None:
    case = load_case("concurrency", "C2")
    assert judge(watcher, case).state == "pass"
    code, _polls, cancels = run_watch(watcher, tmp_path, monkeypatch, [case], case["required"])
    assert code == watcher.EXIT_GREEN
    assert cancels == []


def test_c3_replacement_fails_and_cancels_only_it(watcher, tmp_path, monkeypatch) -> None:
    case = load_case("concurrency", "C3")
    verdict = judge(watcher, case)
    assert verdict.state == "fail"
    assert [j.name for j in verdict.failing] == ["Static checks"]
    assert verdict.failing_run_ids == {CI_YML: 101}
    # Run 100 is completed and run 102 is newer than the failing run.
    assert scoped_cancel(watcher, monkeypatch, case, verdict.failing_run_ids) == [101]


def test_c3_watch_exits_one_with_the_scoped_cancel(watcher, tmp_path, monkeypatch) -> None:
    case = load_case("concurrency", "C3")
    code, _polls, cancels = run_watch(watcher, tmp_path, monkeypatch, [case], case["required"])
    assert code == watcher.EXIT_REQUIRED_FAIL
    assert [c["kwargs"]["scope"] for c in cancels] == [{CI_YML: 101}]


def test_c4_cancelled_with_queued_replacement_is_pending(watcher) -> None:
    assert judge(watcher, load_case("concurrency", "C4")).state == "pending"


def test_c5_cancelled_with_no_newer_run_fails(watcher, tmp_path, monkeypatch) -> None:
    case = load_case("concurrency", "C5")
    verdict = judge(watcher, case)
    assert verdict.state == "fail"
    assert verdict.cancellation_derived
    code, _polls, _cancels = run_watch(
        watcher, tmp_path, monkeypatch, [case], case["required"]
    )
    assert code == watcher.EXIT_REQUIRED_FAIL


def test_c6_cancelled_pending_run_is_ignored(watcher) -> None:
    verdict = judge(watcher, load_case("concurrency", "C6"))
    (refresh,) = by_name(verdict, "refresh")
    assert refresh.check_run_id == 9101  # A, still running, is what is judged
    assert refresh.state == "pending"
    assert verdict.failing == []


def test_c7_job_level_cancel_replaced_by_newer_green_job(watcher) -> None:
    verdict = judge(watcher, load_case("concurrency", "C7"))
    assert verdict.state == "pass"
    assert by_name(verdict, "J")[0].check_run_id == 9003


def test_c8_job_level_cancel_without_newer_job_fails(watcher) -> None:
    verdict = judge(watcher, load_case("concurrency", "C8"))
    assert verdict.state == "fail"
    assert [j.name for j in verdict.failing] == ["J"]


def test_c9_no_concurrency_newest_run_per_check_counts(watcher) -> None:
    verdict = judge(watcher, load_case("concurrency", "C9"))
    assert verdict.state == "pass"
    assert by_name(verdict, "test")[0].check_run_id == 9002


def test_c10_same_job_name_in_two_workflows_is_judged_separately(watcher) -> None:
    verdict = judge(watcher, load_case("concurrency", "C10"))
    tests = by_name(verdict, "test")
    assert sorted(j.workflow for j in tests) == [CI_YML, ".github/workflows/lint.yml"]
    assert verdict.state == "fail"
    assert [j.workflow for j in verdict.failing] == [CI_YML]


def test_c11_dispatch_and_pr_runs_newest_counts_and_mismatch_logged(watcher) -> None:
    verdict = judge(watcher, load_case("concurrency", "C11"))
    assert verdict.state == "pass"
    assert any("workflow_dispatch" in note and "pull_request" in note for note in verdict.notes)


# ---- R: re-runs ----


def test_r1_rerun_all_green_passes(watcher) -> None:
    assert judge(watcher, load_case("reruns", "R1")).state == "pass"


def test_r2_rerun_failed_jobs_green_passes(watcher) -> None:
    assert judge(watcher, load_case("reruns", "R2")).state == "pass"


def test_r3_rerun_in_progress_is_pending(watcher) -> None:
    verdict = judge(watcher, load_case("reruns", "R3"))
    assert verdict.state == "pending"
    assert verdict.failing == []


def test_r4_cancelled_attempt_then_green_attempt_passes(watcher) -> None:
    assert judge(watcher, load_case("reruns", "R4")).state == "pass"


# ---- H: head commit changes ----


def test_h1_old_commits_cancelled_checks_are_ignored(watcher) -> None:
    verdict = judge(watcher, load_case("head", "H1"))
    assert verdict.state == "pass"
    assert [j.check_run_id for j in verdict.judgments] == [9002]


def test_h2_head_change_mid_watch_resets_verdict(watcher, tmp_path, monkeypatch) -> None:
    case = load_case("head", "H2")
    code, polls, cancels = run_watch(
        watcher, tmp_path, monkeypatch, case["polls"], case["required"],
        reread=[case["reread_head"]],
    )
    assert code == watcher.EXIT_GREEN
    assert polls == 2
    assert cancels == []


# ---- T: terminal results ----


@pytest.mark.parametrize("conclusion", ["failure", "timed_out"])
def test_t1_failure_or_timed_out_fails(watcher, conclusion: str) -> None:
    case = load_case("terminal", "T1")
    case["check_runs"][0]["conclusion"] = conclusion
    verdict = judge(watcher, case)
    assert verdict.state == "fail"
    assert not verdict.cancellation_derived


def test_t2_startup_failure_fails_as_a_broken_workflow(watcher) -> None:
    verdict = judge(watcher, load_case("terminal", "T2"))
    assert verdict.state == "fail"
    assert [j.workflow_broken for j in verdict.failing] == [True]


def test_t3_action_required_is_approval_required(watcher) -> None:
    assert judge(watcher, load_case("terminal", "T3")).state == "approval_required"


def test_t4_skipped_not_required_is_ignored(watcher) -> None:
    verdict = judge(watcher, load_case("terminal", "T4"))
    assert verdict.state == "pass"
    (docs,) = by_name(verdict, "docs")
    assert not docs.required


def test_t5_skipped_required_passes(watcher) -> None:
    assert judge(watcher, load_case("terminal", "T5")).state == "pass"


def test_t6_neutral_passes(watcher) -> None:
    assert judge(watcher, load_case("terminal", "T6")).state == "pass"


def test_t7_stale_is_stale_not_pending(watcher) -> None:
    assert judge(watcher, load_case("terminal", "T7")).state == "stale"


def test_t8_always_gate_failure_from_a_superseded_run_is_ignored(watcher) -> None:
    verdict = judge(watcher, load_case("terminal", "T8"))
    assert verdict.state == "pending"
    assert verdict.failing == []
    assert by_name(verdict, "CI OK")[0].state == "pending"


# ---- S: non-Actions sources ----


def test_s1_commit_status_newest_per_context_counts(watcher) -> None:
    verdict = judge(watcher, load_case("sources", "S1"))
    assert verdict.state == "pass"
    (rabbit,) = by_name(verdict, "CodeRabbit")
    assert rabbit.state == "pass"


def test_s2_merge_group_run_is_not_part_of_the_verdict(watcher) -> None:
    verdict = judge(watcher, load_case("sources", "S2"))
    assert verdict.state == "pass"
    assert [j.check_run_id for j in verdict.judgments] == [9001]


# ---- A: review findings ----


def test_a1_rerun_of_older_run_failing_again_fails(watcher) -> None:
    verdict = judge(watcher, load_case("review", "A1"))
    assert verdict.state == "fail"
    assert verdict.failing[0].check_run_id == 9003


def test_a2_rerun_of_older_run_green_passes(watcher) -> None:
    assert judge(watcher, load_case("review", "A2")).state == "pass"


def test_a3_newer_skip_does_not_hide_older_failure(watcher) -> None:
    verdict = judge(watcher, load_case("review", "A3"))
    assert verdict.state == "fail"
    assert verdict.failing[0].check_run_id == 9001


def test_a4_newer_skip_replaces_older_cancellation(watcher) -> None:
    assert judge(watcher, load_case("review", "A4")).state == "pass"


def test_a5_cancelled_run_on_old_head_restarts_on_new_head(
    watcher, tmp_path, monkeypatch
) -> None:
    case = load_case("review", "A5")
    code, polls, cancels = run_watch(
        watcher, tmp_path, monkeypatch, case["polls"], case["required"],
        reread=[case["reread_head"]],
    )
    assert code == watcher.EXIT_GREEN
    assert polls == 2
    assert cancels == []


def test_a6_required_check_never_reported_exits_six(watcher, tmp_path, monkeypatch) -> None:
    case = load_case("review", "A6")
    verdict = judge(watcher, case)
    assert verdict.state == "never_reported"
    assert verdict.missing == ["docs"]
    code, _polls, _cancels = run_watch(
        watcher, tmp_path, monkeypatch, [case], case["required"]
    )
    assert code == watcher.EXIT_NEVER_REPORTED


def test_a7_two_files_with_the_same_display_name_are_judged_separately(watcher) -> None:
    verdict = judge(watcher, load_case("review", "A7"))
    assert sorted(j.workflow for j in by_name(verdict, "test")) == [
        ".github/workflows/ci-nightly.yml",
        CI_YML,
    ]
    assert verdict.state == "fail"


def test_a8_every_page_is_read_and_the_newest_wins(watcher, monkeypatch) -> None:
    case = load_case("review", "A8")
    filler = case["page1_filler"]
    page1 = [case["page1_first"]] + [
        {**filler, "id": filler["id"] + n, "name": f"shard-{n}"} for n in range(99)
    ]
    requested: list[str] = []

    def fake_gh_json(*args):
        path = args[-1]
        requested.append(path)
        if "/check-runs" in path:
            page = 1 if path.endswith("page=1") else 2
            return {"total_count": 101, "check_runs": page1 if page == 1 else case["page2"]}
        return {"total_count": 2, "workflow_runs": case["workflow_runs"]}

    monkeypatch.setattr(watcher, "gh_json", fake_gh_json)
    head = watcher.fetch_head_checks("zackees/clud", case["head_sha"])
    assert head is not None
    assert len(head.check_runs) == 101
    assert any("/check-runs" in p and p.endswith("page=2") for p in requested)
    assert any("filter=all" in p for p in requested)
    verdict = watcher.judge_check_runs(
        head.check_runs, head.workflow_runs, case["head_sha"], set(case["required"])
    )
    assert verdict.state == "pass"
    assert by_name(verdict, "linux")[0].check_run_id == 9500


def test_a9_action_required_exits_five_immediately(watcher, tmp_path, monkeypatch) -> None:
    case = load_case("review", "A9")
    assert judge(watcher, case).state == "approval_required"
    code, polls, _cancels = run_watch(
        watcher, tmp_path, monkeypatch, [case], case["required"]
    )
    assert code == watcher.EXIT_APPROVAL_REQUIRED
    assert polls == 1


def test_a10_stale_exits_seven_with_rerun_needed(
    watcher, tmp_path, monkeypatch, capsys
) -> None:
    case = load_case("review", "A10")
    code, _polls, _cancels = run_watch(
        watcher, tmp_path, monkeypatch, [case], case["required"]
    )
    assert code == watcher.EXIT_STALE
    assert "re-run needed" in capsys.readouterr().out


def test_a11_required_skipped_passes(watcher) -> None:
    assert judge(watcher, load_case("review", "A11")).state == "pass"


def test_a12_fail_fast_siblings_are_not_reported(watcher) -> None:
    verdict = judge(watcher, load_case("review", "A12"))
    assert verdict.state == "fail"
    assert [j.name for j in verdict.failing] == ["L1"]


def test_a13_other_prs_run_on_the_shared_commit_is_ignored(watcher) -> None:
    case = load_case("review", "A13")
    verdict = judge(watcher, case)
    assert verdict.state == "pass"
    assert [j.check_run_id for j in verdict.judgments] == [9001]


def test_a14_cancel_scope_is_the_failing_workflow_at_or_below_the_run(
    watcher, monkeypatch
) -> None:
    case = load_case("review", "A14")
    verdict = judge(watcher, case)
    assert verdict.state == "fail"
    assert verdict.failing_run_ids == {CI_YML: 200}
    # 201 is newer than the failing run; 300 is another workflow.
    assert scoped_cancel(watcher, monkeypatch, case, verdict.failing_run_ids) == [198, 200]
