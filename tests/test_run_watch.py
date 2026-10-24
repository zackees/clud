"""Unit coverage for the bundled `run_watch.py` waiter (#1941)."""

from __future__ import annotations

import sys
from pathlib import Path

TOOL_DIR = (
    Path(__file__).resolve().parents[1] / "crates" / "clud-bin" / "assets" / "tools" / "github"
)


def _load():
    if str(TOOL_DIR) not in sys.path:
        sys.path.insert(0, str(TOOL_DIR))
    import run_watch

    return run_watch


run_watch = _load()


def _job(name: str, status: str, conclusion: str = "") -> dict:
    return {"name": name, "status": status, "conclusion": conclusion}


def test_a_failed_job_fails_fast_while_others_still_run() -> None:
    run = {
        "status": "in_progress",
        "jobs": [_job("build", "completed", "failure"), _job("test", "in_progress")],
    }
    state, failed = run_watch.evaluate(run)
    assert state == "failed"
    assert [job["name"] for job in failed] == ["build"]


def test_an_in_progress_run_without_failures_is_pending() -> None:
    run = {"status": "in_progress", "jobs": [_job("build", "completed", "success")]}
    assert run_watch.evaluate(run) == ("pending", [])


def test_a_clean_completed_run_is_success_and_skipped_jobs_do_not_fail_it() -> None:
    run = {
        "status": "completed",
        "conclusion": "success",
        "jobs": [_job("build", "completed", "success"), _job("mac", "completed", "skipped")],
    }
    assert run_watch.evaluate(run) == ("success", [])


def test_a_run_that_concluded_badly_with_no_failed_job_still_fails() -> None:
    run = {"status": "completed", "conclusion": "cancelled", "jobs": []}
    assert run_watch.evaluate(run) == ("failed", [])
