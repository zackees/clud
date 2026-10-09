"""obs-rust/obs-studio#13: a failure must not discard in-flight sibling jobs.

During a /grind run on obs-rust/obs-studio, one job of the PR's CI run failed
while the other jobs (full OBS builds, an hour in) were still running. The
watch exited 1 and cancelled the whole run. GitHub cancels runs, not jobs, so
every sibling build was lost with its timing and result.

The failing run is now cancelled only when none of its jobs is still in
progress. Older runs of the same workflow (superseded by the failing run)
are still cancelled, and a run whose remaining jobs are only queued is still
cancelled, since dropping it loses nothing.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "github" / "pr_merge_watch.py"

REPO = "obs-rust/obs-studio"
SHA = "c0ffee" * 6 + "c0ff"
WORKFLOW = ".github/workflows/pr.yaml"
OLDER_RUN = 100  # an older run of the same workflow on the head: superseded
FAILING_RUN = 101  # the run with the failed job


@pytest.fixture
def watcher():
    name = "clud_test_pr_merge_watch_inflight_cancel"
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


def _run(run_id: int, created_at: str) -> dict:
    return {
        "id": run_id,
        "status": "in_progress",
        "head_sha": SHA,
        "path": WORKFLOW,
        "created_at": created_at,
    }


RUNS = [_run(OLDER_RUN, "2026-10-08T20:00:00Z"), _run(FAILING_RUN, "2026-10-08T20:05:00Z")]


def _job(job_id: int, status: str, conclusion: str | None = None) -> dict:
    return {"id": job_id, "status": status, "conclusion": conclusion}


def _cancel(watcher, monkeypatch, jobs: list[dict] | None) -> list[int]:
    """Run the fail-scoped cancel; return the run ids it POSTed a cancel for."""

    def fake_gh_json(*args):
        path = args[-1]
        if "/actions/runs?" in path:
            return {"workflow_runs": RUNS}
        if f"/actions/runs/{FAILING_RUN}/jobs" in path:
            return None if jobs is None else {"jobs": jobs}
        return {"jobs": []}

    monkeypatch.setattr(watcher, "gh_json", fake_gh_json)
    cancelled: list[int] = []

    def fake_gh(*args, **_kwargs):
        cancelled.append(int(args[-1].split("/")[-2]))
        return watcher.GhResult(0, "", "")

    monkeypatch.setattr(watcher, "gh", fake_gh)
    opts = watcher.CancelOptions({"fail"}, "runs", 30, False, False, True, False)
    watcher.cancel_pr_runs(13, REPO, SHA, opts, None, scope={WORKFLOW: FAILING_RUN})
    return cancelled


def test_failing_run_with_a_job_in_progress_is_not_cancelled(watcher, monkeypatch) -> None:
    jobs = [
        _job(1, "completed", "failure"),  # the first failing job
        _job(2, "in_progress"),  # a full build still running
        _job(3, "queued"),
    ]
    assert _cancel(watcher, monkeypatch, jobs) == [OLDER_RUN]


def test_failing_run_with_only_queued_jobs_left_is_cancelled(watcher, monkeypatch) -> None:
    jobs = [_job(1, "completed", "failure"), _job(2, "completed", "success"), _job(3, "queued")]
    assert _cancel(watcher, monkeypatch, jobs) == [OLDER_RUN, FAILING_RUN]


def test_failing_run_is_kept_when_its_jobs_cannot_be_read(watcher, monkeypatch) -> None:
    assert _cancel(watcher, monkeypatch, None) == [OLDER_RUN]
