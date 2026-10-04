"""Bosn's expanded jobs, not skipped matrix planning nodes, prove gate lanes."""

from __future__ import annotations

from copy import deepcopy
from dataclasses import dataclass

import pytest

from ci.local_gate import ROOT, proved_lanes

HEAD = "a" * 40
EXPECTED = {
    "CI/Static checks": ("Static checks",),
    "Dylint/Dylint/Dylint": (
        "Check dylint formatting", "Test dylint crate", "Run dylint over workspace",
        "Run dylint for x86_64-pc-windows-msvc", "Run dylint for aarch64-apple-darwin",
    ),
    "Clippy linux-x64/Build target/x86_64-unknown-linux-gnu": ("Clippy", "Doc tests"),
    "Build linux-x64/Build target/x86_64-unknown-linux-gnu": (
        "Build binaries and test harnesses", "Build wheel", "Pack bundle", "Upload bundle",
    ),
    **{f"Test linux-x64 (unit)-{i}/Run tests/x86_64-unknown-linux-gnu unit": ("Run unit suite",)
       for i in range(1, 4)},
    "CI/CI OK": ("Check results",),
}


def _record() -> dict[str, object]:
    jobs = [
        {
            "key": key, "status": "completed", "conclusion": "success",
            "sections": [
                {"name": step, "stage": "Main", "status": "completed",
                 "conclusion": "success"}
                for step in steps
            ],
        }
        for key, steps in EXPECTED.items()
    ]
    # Bosn also reports skipped outer matrix nodes; they must not veto actual
    # completed expanded jobs or count as proof themselves.
    jobs.append({
        "key": "Test linux-x64 (unit)", "status": "completed",
        "conclusion": "skipped", "sections": [],
    })
    return {
        "schema_version": 1, "repository": "zackees/clud",
        "workflow": ".github/workflows/ci.yml", "trigger": "pr", "mode": "minimal",
        "sha": HEAD, "state": "done", "conclusion": "success",
        "exit_code": 0, "act_exit_code": 0, "dirty": None,
        "tree_digest": "b" * 64, "job": None,
        "workspace": str(ROOT), "engine": "act", "act_version": "0.2.89-act2.3",
        "tree": {"malformed_lines": 0, "groups": [{"jobs": jobs}]},
    }


def test_full_run_requires_each_expanded_job_and_required_step() -> None:
    record = _record()
    assert proved_lanes(record, head=HEAD, lane=None) == (
        "static", "dylint", "clippy", "build", "unit"
    )
    jobs = record["tree"]["groups"][0]["jobs"]  # type: ignore[index]
    jobs.pop(2)
    with pytest.raises(ValueError, match="clippy"):
        proved_lanes(record, head=HEAD, lane=None)


def test_missing_unit_shard_or_skipped_dylint_step_fails_closed() -> None:
    record = _record()
    jobs = record["tree"]["groups"][0]["jobs"]  # type: ignore[index]
    jobs.pop(6)  # one of three unit shards
    with pytest.raises(ValueError, match="unit"):
        proved_lanes(record, head=HEAD, lane=None)

    record = _record()
    jobs = record["tree"]["groups"][0]["jobs"]  # type: ignore[index]
    jobs[1]["sections"][-1]["conclusion"] = "skipped"
    with pytest.raises(ValueError, match="dylint"):
        proved_lanes(record, head=HEAD, lane=None)


def test_wrong_head_dirty_snapshot_and_wrong_selected_job_fail_closed() -> None:
    record = _record()
    with pytest.raises(ValueError, match="metadata"):
        proved_lanes(record, head="c" * 40, lane=None)
    dirty = deepcopy(record)
    dirty["dirty"] = "b" * 64
    with pytest.raises(ValueError, match="clean"):
        proved_lanes(dirty, head=HEAD, lane=None)
    with pytest.raises(ValueError, match="different job"):
        proved_lanes(record, head=HEAD, lane="dylint")


@dataclass(frozen=True)
class InvalidMetadata:
    field: str
    value: str | bool


@pytest.mark.parametrize("case", [
    InvalidMetadata("engine", "native"),
    InvalidMetadata("workspace", str(ROOT.parent / "another-checkout")),
    InvalidMetadata("act_version", "0.2.89-act2.2"),
    InvalidMetadata("act_version", "0.2.89"),
    InvalidMetadata("schema_version", True),
    InvalidMetadata("exit_code", False),
    InvalidMetadata("act_exit_code", False),
])
def test_untrusted_engine_workspace_version_and_boolean_metadata_fail_closed(
    case: InvalidMetadata,
) -> None:
    record = _record()
    record[case.field] = case.value
    with pytest.raises(ValueError, match="Bosn"):
        proved_lanes(record, head=HEAD, lane=None)


@pytest.mark.parametrize("stage", ["Pre", "Post", "Complete", None])
def test_required_step_must_have_executed_in_main_stage(stage: str | None) -> None:
    record = _record()
    jobs = record["tree"]["groups"][0]["jobs"]  # type: ignore[index]
    jobs[1]["sections"][-1]["stage"] = stage
    with pytest.raises(ValueError, match="dylint"):
        proved_lanes(record, head=HEAD, lane=None)


def test_boolean_malformed_line_count_fails_closed() -> None:
    record = _record()
    record["tree"]["malformed_lines"] = False  # type: ignore[index]
    with pytest.raises(ValueError, match="malformed"):
        proved_lanes(record, head=HEAD, lane=None)
