"""ci/cancel_superseded.py: which merge-queue runs a newer one replaced (#2048)."""

from __future__ import annotations

from pathlib import Path

import pytest

from ci.cancel_superseded import queue_pr, superseded

ROOT = Path(__file__).resolve().parent.parent


@pytest.mark.parametrize(
    ("ref", "expected"),
    [
        ("gh-readonly-queue/main/pr-2048-0123abcd", 2048),
        ("refs/heads/gh-readonly-queue/main/pr-7-ff", 7),
        ("main", None),
        ("feat/pr-12-abc", None),
        ("gh-readonly-queue/main/pr-x-abc", None),
    ],
)
def test_queue_pr(ref: str, expected: int | None) -> None:
    assert queue_pr(ref) == expected


def run(run_id: int, pr: int, created: str, status: str = "in_progress") -> dict:
    return {
        "id": run_id,
        "head_branch": f"gh-readonly-queue/main/pr-{pr}-{run_id:08x}",
        "created_at": created,
        "status": status,
    }


def test_only_older_live_runs_of_the_same_pr_are_superseded() -> None:
    current = run(10, 5, "2026-10-24T12:00:00Z")
    runs = [
        current,
        run(1, 5, "2026-10-24T11:00:00Z"),
        run(2, 5, "2026-10-24T11:30:00Z", status="queued"),
        run(3, 5, "2026-10-24T11:40:00Z", status="completed"),
        run(4, 6, "2026-10-24T11:00:00Z"),
        run(11, 5, "2026-10-24T12:05:00Z"),
        run(1, 5, "2026-10-24T11:00:00Z"),
    ]
    assert superseded(runs, current) == [1, 2]


def test_a_non_queue_run_supersedes_nothing() -> None:
    current = {"id": 1, "head_branch": "main", "created_at": "2026-10-24T12:00:00Z"}
    assert superseded([run(2, 5, "2026-10-24T11:00:00Z")], current) == []


def test_ci_runs_the_canceller_only_for_merge_groups() -> None:
    workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
    job = workflow.split("\n  cancel-superseded:\n", 1)[1].split("\n  # ---", 1)[0]
    assert "github.event_name == 'merge_group'" in job
    assert "python -m ci.cancel_superseded" in job
    assert "actions: write" in job
