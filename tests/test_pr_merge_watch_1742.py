"""#1742: a watch must never cancel a run other than the one that failed.

On FastLED/fbuild#1624 two `ci-minimal` runs started for one head SHA, and
concurrency cancelled the older one, whose aggregate jobs then reported
`failure`. Two paths could turn that into a cancelled live run:

- The rollup fallback (no REST check-run data for a poll) judged every rollup
  row without the supersession rule and cancelled every running run on the
  head. A failure verdict must come from `judge_check_runs`, the one rule.
- The `gh pr checks --watch` shim upgrade ran the watcher with its default
  `--cancel-on fail,review,closed`, so an unrelated CodeRabbit thread
  cancelled the whole matrix (the recorded watch log shows exactly that). The
  shim side is covered in `tests/test_gh_shim.py`.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "github" / "pr_merge_watch.py"

REPO = "FastLED/fbuild"
SHA = "6adb7dcab5043a385a05b5d6bde3521da5a4a893"
WORKFLOW = ".github/workflows/ci-minimal.yml"
STALE_RUN = 37065670437  # cancelled by concurrency
LIVE_RUN = 37065670644  # the run that must survive
AGGREGATE = "CI selected coverage"


@pytest.fixture
def watcher():
    name = "clud_test_pr_merge_watch_1742"
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


def _link(run_id: int, job_id: int) -> str:
    return f"https://github.com/{REPO}/actions/runs/{run_id}/job/{job_id}"


def _run(run_id: int, number: int, status: str, conclusion: str | None) -> dict:
    return {
        "id": run_id,
        "run_number": number,
        "path": WORKFLOW,
        "head_sha": SHA,
        "head_branch": "feat",
        "event": "pull_request",
        "status": status,
        "conclusion": conclusion,
        # Same second: `opened` and `labeled` fired together (#1710).
        "created_at": "2026-10-02T21:14:49Z",
    }


def _runs(live_status: str, live_conclusion: str | None) -> list[dict]:
    """The cancelled stale run and the live run, in the given state."""
    return [
        _run(STALE_RUN, 444, "completed", "cancelled"),
        _run(LIVE_RUN, 445, live_status, live_conclusion),
    ]


def _check(check_id: int, name: str, run_id: int, status: str, conclusion: str | None) -> dict:
    return {
        "id": check_id,
        "name": name,
        "head_sha": SHA,
        "status": status,
        "conclusion": conclusion,
        "details_url": _link(run_id, check_id),
    }


def _stale_checks() -> list[dict]:
    return [
        _check(1, "build", STALE_RUN, "completed", "cancelled"),
        _check(2, AGGREGATE, STALE_RUN, "completed", "failure"),
    ]


def _gate(watcher, head_checks):
    rollup = [
        watcher.CheckRow("build", "cancel", "CANCELLED", _link(STALE_RUN, 1)),
        watcher.CheckRow(AGGREGATE, "fail", "FAILURE", _link(STALE_RUN, 2)),
        watcher.CheckRow("build", "pending", "IN_PROGRESS", _link(LIVE_RUN, 3)),
    ]
    return watcher.GateSnapshot(
        pr=watcher.PRSnapshot(1624, "OPEN", "MERGEABLE", SHA, "main", "feat", "BLOCKED"),
        checks=rollup,
        human_review_ids=frozenset(),
        coderabbit_probe=watcher.CodeRabbitProbe("not_detected", 0),
        coderabbit=watcher.CodeRabbitObservation("quiet"),
        head_checks=head_checks,
    )


def _drive(watcher, monkeypatch, gates: list) -> tuple[int, list, int]:
    monkeypatch.setattr(
        watcher.PRSnapshot,
        "fetch",
        lambda *_a: watcher.PRSnapshot(1624, "OPEN", "MERGEABLE", SHA, "main", "feat"),
    )
    monkeypatch.setattr(watcher, "fetch_required_check_names", lambda *_a: None)
    monkeypatch.setattr(watcher, "emit_progress_report", lambda *_a, **_k: None)
    monkeypatch.setattr(watcher, "_sleep_remaining_interval", lambda *_a: None)
    monkeypatch.setattr(
        watcher,
        "_build_failure_report",
        lambda check, _repo: watcher.FailureReport(check, None, "", None),
    )
    cancels: list = []
    monkeypatch.setattr(
        watcher, "cancel_pr_runs", lambda *a, **k: cancels.append((a, k)) or 1
    )
    polls = {"n": 0}

    def fetch_gate(*_a, **_k):
        index = min(polls["n"], len(gates) - 1)
        polls["n"] += 1
        return gates[index]

    monkeypatch.setattr(watcher, "fetch_gate_snapshot", fetch_gate)
    opts = watcher.CancelOptions({"fail"}, "runs", 30, False, False, True, False)
    with pytest.raises(SystemExit) as exc:
        watcher.watch(1624, REPO, 20, 3600, None, opts, None)
    return exc.value.code, cancels, polls["n"]


def test_stale_aggregate_in_the_rollup_fallback_never_cancels_the_live_run(
    watcher, monkeypatch
) -> None:
    """RED before #1742: the first (rollup-only) poll exited 1 and cancelled
    every running run on the head, the live run included."""
    live_running = watcher.HeadChecks(
        [*_stale_checks(), _check(3, "build", LIVE_RUN, "in_progress", None)],
        _runs("in_progress", None),
    )
    live_green = watcher.HeadChecks(
        [
            *_stale_checks(),
            _check(3, "build", LIVE_RUN, "completed", "success"),
            _check(4, AGGREGATE, LIVE_RUN, "completed", "success"),
        ],
        _runs("completed", "success"),
    )

    code, cancels, polls = _drive(
        watcher,
        monkeypatch,
        [_gate(watcher, None), _gate(watcher, live_running), _gate(watcher, live_green)],
    )

    assert cancels == []
    assert code == watcher.EXIT_GREEN
    assert polls == 3


def test_rollup_failure_without_run_data_never_becomes_a_verdict(watcher, monkeypatch) -> None:
    """A rollup cannot tell a superseded run's check from a live one, so it
    never decides a failure: persistent missing run data is `unreachable`,
    and nothing is cancelled."""
    code, cancels, polls = _drive(watcher, monkeypatch, [_gate(watcher, None)])

    assert cancels == []
    assert code == watcher.EXIT_GITHUB_UNREACHABLE
    assert polls == watcher.MAX_CONSECUTIVE_API_FAILURES


def test_judged_stale_aggregate_keeps_watching_and_cancels_nothing(watcher) -> None:
    """The shared rule itself: the superseded run's failed aggregate is pending
    on the newer in-progress run, never a failure with a cancel scope."""
    verdict = watcher.judge_check_runs(
        [*_stale_checks(), _check(3, "build", LIVE_RUN, "in_progress", None)],
        _runs("in_progress", None),
        SHA,
        None,
        head_branch="feat",
    )
    assert verdict.state == "pending"
    assert verdict.failing == []
    assert verdict.failing_run_ids == {}
