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

    def fake_watch(*_args) -> int:
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
