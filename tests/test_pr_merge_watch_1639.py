"""pr_merge_watch: green needs a real result on the head SHA (issue #1639).

On PR #1638 the watcher reported GREEN while the head commit's CI run was
still queued: the jobs gated off by `if:` completed instantly as `skipped`,
the `CI OK` aggregate had not been created yet (it `needs:` the queued
jobs), and a skipped check counted as a pass. The lander merges on GREEN, so
skipped-only results, or results on another SHA, must read as pending.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "github" / "pr_merge_watch.py"

HEAD = "a" * 40
OLD = "b" * 40
CI = ".github/workflows/ci.yml"


@pytest.fixture
def watcher():
    name = "clud_test_pr_merge_watch_1639"
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


def _run(run_id: int, sha: str, status: str, conclusion: str | None = None) -> dict:
    return {
        "id": run_id,
        "head_sha": sha,
        "head_branch": "fix",
        "event": "pull_request",
        "path": CI,
        "status": status,
        "conclusion": conclusion,
        "run_attempt": 1,
        "check_suite_id": run_id * 10,
    }


def _check(check_id: int, name: str, sha: str, run_id: int, conclusion: str | None) -> dict:
    return {
        "id": check_id,
        "name": name,
        "head_sha": sha,
        "status": "completed" if conclusion else "queued",
        "conclusion": conclusion,
        "details_url": f"https://github.com/o/r/actions/runs/{run_id}/job/{check_id}",
        "check_suite": {"id": run_id * 10},
    }


SKIPPED_LANES = ["build (windows)", "test (macos)", "dylint"]


@pytest.mark.parametrize("required", [None, set(), {"CI OK"}])
def test_only_skipped_checks_on_head_is_not_green(watcher, required) -> None:
    runs = [_run(2, HEAD, "queued")]
    checks = [_check(10 + i, n, HEAD, 2, "skipped") for i, n in enumerate(SKIPPED_LANES)]
    verdict = watcher.judge_check_runs(checks, runs, HEAD, required, head_branch="fix")
    assert verdict.state == "pending"


def test_only_skipped_checks_on_head_with_completed_run_is_not_green(watcher) -> None:
    runs = [_run(2, HEAD, "completed", "success")]
    checks = [_check(10 + i, n, HEAD, 2, "skipped") for i, n in enumerate(SKIPPED_LANES)]
    verdict = watcher.judge_check_runs(checks, runs, HEAD, set(), head_branch="fix")
    assert verdict.state != "pass"


@pytest.mark.parametrize("required", [None, set(), {"CI OK"}])
def test_green_checks_on_older_sha_and_nothing_on_head_is_not_green(watcher, required) -> None:
    runs = [_run(1, OLD, "completed", "success"), _run(2, HEAD, "queued")]
    checks = [
        _check(1, "CI OK", OLD, 1, "success"),
        _check(2, "build (linux)", OLD, 1, "success"),
    ]
    verdict = watcher.judge_check_runs(checks, runs, HEAD, required, head_branch="fix")
    assert verdict.state == "pending"


@pytest.mark.parametrize("required", [None, set(), {"CI OK"}])
def test_ci_ok_success_on_head_with_skipped_lanes_is_green(watcher, required) -> None:
    runs = [_run(2, HEAD, "completed", "success")]
    checks = [_check(10 + i, n, HEAD, 2, "skipped") for i, n in enumerate(SKIPPED_LANES)]
    checks.append(_check(20, "build (linux)", HEAD, 2, "success"))
    checks.append(_check(21, "CI OK", HEAD, 2, "success"))
    verdict = watcher.judge_check_runs(checks, runs, HEAD, required, head_branch="fix")
    assert verdict.state == "pass"


def test_required_ci_ok_skipped_on_head_is_not_green(watcher) -> None:
    runs = [_run(2, HEAD, "completed", "success")]
    checks = [
        _check(20, "build (linux)", HEAD, 2, "success"),
        _check(21, "CI OK", HEAD, 2, "skipped"),
    ]
    verdict = watcher.judge_check_runs(checks, runs, HEAD, {"CI OK"}, head_branch="fix")
    assert verdict.state != "pass"


def test_failure_detection_unchanged(watcher) -> None:
    runs = [_run(2, HEAD, "in_progress")]
    checks = [
        _check(10, "dylint", HEAD, 2, "skipped"),
        _check(11, "build (linux)", HEAD, 2, "failure"),
    ]
    verdict = watcher.judge_check_runs(checks, runs, HEAD, set(), head_branch="fix")
    assert verdict.state == "fail"
