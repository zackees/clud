"""pr_merge_watch: an unreadable failure log is explained, not silent (issue #1631).

When the failed job's log fetch returns nothing (the bounded probe times out,
the endpoint answers with an empty body, or the run-level fallback is refused
because the run is still in progress), the report must carry an explicit
`log unavailable: <reason>` line. A concluded job whose log is empty gets one
bounded retry. A readable log is unaffected, and the check stays a failure
either way.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "github" / "pr_merge_watch.py"

LINK = "https://github.com/o/r/actions/runs/111/job/222"
JOB_PATH = "repos/o/r/actions/jobs/222/logs"
IN_PROGRESS = "run 111 is still in progress; logs will be available when it is complete"
NORMAL_LOG = "2026-09-30T04:02:32.2188931Z FAILED tests/test_x.py::test_y - assert False\n"


@pytest.fixture
def watcher():
    name = "clud_test_pr_merge_watch_1631"
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


class FakeGh:
    """Stands in for `gh`: answers job-log and run-log calls from queues."""

    def __init__(self, watcher, job: list, run: list) -> None:
        self.watcher = watcher
        self.job = list(job)
        self.run = list(run)
        self.calls: list[tuple[str, ...]] = []

    def __call__(self, *args: str, check: bool = False, timeout: float | None = None):
        self.calls.append(args)
        queue = self.job if JOB_PATH in args else self.run
        exit_code, stdout, stderr = queue.pop(0) if len(queue) > 1 else queue[0]
        return self.watcher.GhResult(exit_code, stdout, stderr)

    def job_calls(self) -> int:
        return sum(1 for c in self.calls if JOB_PATH in c)


def _report(watcher, monkeypatch, job: list, run: list):
    fake = FakeGh(watcher, job, run)
    monkeypatch.setattr(watcher, "gh", fake)
    sleeps: list[float] = []
    monkeypatch.setattr(watcher.time, "sleep", sleeps.append)
    check = watcher.CheckRow(name="unit", bucket="fail", state="FAILURE", link=LINK)
    report = watcher._build_failure_report(check, "o/r")
    return report, fake, sleeps


def test_timeout_and_refused_fallback_print_log_unavailable(watcher, monkeypatch) -> None:
    report, _, _ = _report(
        watcher,
        monkeypatch,
        job=[(124, "", "gh api timed out after 25.0s")],
        run=[(1, "", IN_PROGRESS)],
    )
    text = report.render()
    assert "log unavailable:" in text
    assert "timed out" in text
    assert "still in progress" in text
    assert report.check.bucket == "fail"
    assert "first error:" not in text


def test_empty_job_log_is_retried_once_then_explained(watcher, monkeypatch) -> None:
    report, fake, sleeps = _report(
        watcher, monkeypatch, job=[(0, "", "")], run=[(1, "", IN_PROGRESS)]
    )
    assert fake.job_calls() == 2
    assert len(sleeps) == 1
    assert 0 < sleeps[0] <= 10
    text = report.render()
    assert "log unavailable:" in text
    assert "empty" in text


def test_empty_job_log_recovers_on_retry(watcher, monkeypatch) -> None:
    report, fake, _ = _report(
        watcher, monkeypatch, job=[(0, "", ""), (0, NORMAL_LOG, "")], run=[(1, "", IN_PROGRESS)]
    )
    assert fake.job_calls() == 2
    assert report.first_error == "FAILED tests/test_x.py::test_y - assert False"
    assert "log unavailable" not in report.render()


def test_normal_log_is_unaffected(watcher, monkeypatch) -> None:
    report, fake, sleeps = _report(
        watcher, monkeypatch, job=[(0, NORMAL_LOG, "")], run=[(1, "", IN_PROGRESS)]
    )
    assert fake.job_calls() == 1
    assert sleeps == []
    text = report.render()
    assert "first error: FAILED tests/test_x.py::test_y - assert False" in text
    assert "log unavailable" not in text
