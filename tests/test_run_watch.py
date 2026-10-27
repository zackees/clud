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


def test_failed_steps_names_only_the_steps_that_concluded_badly() -> None:
    job = {
        "name": "build",
        "steps": [
            {"name": "checkout", "conclusion": "success"},
            {"name": "compile", "conclusion": "failure"},
            {"name": "upload", "conclusion": "skipped"},
        ],
    }
    assert run_watch.failed_steps(job) == ["compile"]
    assert run_watch.failed_steps({"name": "x"}) == []


def test_log_tail_keeps_the_last_non_empty_lines() -> None:
    log = "\n".join(f"line {i}" for i in range(100)) + "\n\n"
    tail = run_watch.log_tail(log, lines=3)
    assert tail == "line 97\nline 98\nline 99"


def test_interval_is_clamped_to_the_minimum() -> None:
    assert run_watch.parse_args(["1", "--interval", "5"]).interval == run_watch.MIN_INTERVAL
    assert run_watch.parse_args(["1", "--interval", "90"]).interval == 90.0


def test_a_failed_job_report_prints_step_and_log_tail(monkeypatch, capsys) -> None:
    calls = []

    def fake_gh(*args):
        calls.append(args)
        return 0, "noise\nerror: boom\n", ""

    monkeypatch.setattr(run_watch, "gh", fake_gh)
    job = {
        "name": "build",
        "databaseId": 42,
        "steps": [{"name": "compile", "conclusion": "failure"}],
    }
    run_watch.report_failed_job(job, "o/r")
    out = capsys.readouterr().out
    assert "FAILED job: build" in out
    assert "failed step(s): compile" in out
    assert "error: boom" in out
    assert calls == [("run", "view", "--job", "42", "--log-failed", "--repo", "o/r")]
