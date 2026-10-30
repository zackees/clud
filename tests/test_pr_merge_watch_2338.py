"""pr_merge_watch: an older run's `CI OK` never stands for a newer head run (#2338).

On PR #2332 `--no-merge-wait` exited GREEN within seconds while the head
commit's newest CI run was still building. An older run of the same workflow
on the same head SHA (a re-triggered `labeled` run) had passed, and the newer
run had not yet created its own `CI OK` check, so the old pass was effective.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "github" / "pr_merge_watch.py"

HEAD = "a" * 40
CI = ".github/workflows/ci.yml"


@pytest.fixture
def watcher():
    name = "clud_test_pr_merge_watch_2338"
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


def _run(run_id: int, status: str, conclusion: str | None = None) -> dict:
    return {
        "id": run_id,
        "run_number": run_id,
        "created_at": f"2026-10-29T10:00:{run_id:02d}Z",
        "head_sha": HEAD,
        "head_branch": "fix",
        "event": "pull_request",
        "path": CI,
        "status": status,
        "conclusion": conclusion,
        "run_attempt": 1,
        "check_suite_id": run_id * 10,
    }


def _check(check_id: int, name: str, run_id: int, status: str, conclusion: str | None) -> dict:
    return {
        "id": check_id,
        "name": name,
        "head_sha": HEAD,
        "status": status,
        "conclusion": conclusion,
        "details_url": f"https://github.com/o/r/actions/runs/{run_id}/job/{check_id}",
        "check_suite": {"id": run_id * 10},
    }


def _judge(watcher, checks, runs, required):
    return watcher.judge_check_runs(checks, runs, HEAD, required, head_branch="fix")


@pytest.mark.parametrize("required", [None, set(), {"CI OK"}])
def test_older_pass_with_newer_in_progress_run_and_no_ci_ok_is_pending(watcher, required) -> None:
    runs = [_run(1, "completed", "success"), _run(2, "in_progress")]
    checks = [
        _check(1, "build", 1, "completed", "success"),
        _check(2, "CI OK", 1, "completed", "success"),
        _check(3, "build", 2, "in_progress", None),
    ]
    assert _judge(watcher, checks, runs, required).state == "pending"


def test_head_ci_ok_success_from_newest_run_is_green(watcher) -> None:
    runs = [_run(1, "completed", "success"), _run(2, "completed", "success")]
    checks = [
        _check(1, "CI OK", 1, "completed", "success"),
        _check(3, "build", 2, "completed", "success"),
        _check(4, "CI OK", 2, "completed", "success"),
    ]
    assert _judge(watcher, checks, runs, {"CI OK"}).state == "pass"


def test_head_ci_ok_failure_from_newest_run_is_failure(watcher) -> None:
    runs = [_run(1, "completed", "success"), _run(2, "completed", "failure")]
    checks = [
        _check(1, "CI OK", 1, "completed", "success"),
        _check(4, "CI OK", 2, "completed", "failure"),
    ]
    assert _judge(watcher, checks, runs, {"CI OK"}).state == "fail"


def test_cancelled_newer_run_does_not_hide_the_older_pass(watcher) -> None:
    runs = [_run(1, "completed", "success"), _run(2, "completed", "cancelled")]
    checks = [
        _check(1, "CI OK", 1, "completed", "success"),
        _check(4, "build", 2, "completed", "cancelled"),
    ]
    assert _judge(watcher, checks, runs, {"CI OK"}).state == "pass"
