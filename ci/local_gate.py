"""Run clud's Bosn PR gate and prove named jobs for GATE-007 receipts.

Bosn's outer matrix planning nodes may be skipped even when their expanded
jobs pass. Inspect the expanded job keys and required steps instead.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
from pathlib import Path

from running_process import EOS, PIPE, RunningProcess

from ci import process

ROOT = Path(__file__).resolve().parent.parent
LANE_JOBS: dict[str, tuple[tuple[str, tuple[str, ...]], ...]] = {
    "static": (("CI/Static checks", ("Static checks",)),),
    "dylint": (("Dylint/Dylint/Dylint", (
        "Check dylint formatting", "Test dylint crate", "Run dylint over workspace",
        "Run dylint for x86_64-pc-windows-msvc", "Run dylint for aarch64-apple-darwin",
    )),),
    "clippy": ((
        "Clippy linux-x64/Build target/x86_64-unknown-linux-gnu", ("Clippy", "Doc tests")
    ),),
    "build": (("Build linux-x64/Build target/x86_64-unknown-linux-gnu", (
        "Build binaries and test harnesses", "Build wheel", "Pack bundle", "Upload bundle",
    )),),
    "unit": tuple((f"Test linux-x64 (unit)-{i}/Run tests/x86_64-unknown-linux-gnu unit",
                   ("Run unit suite",)) for i in range(1, 4)),
}
JOB_IDS = {"static": "static-checks", "dylint": "dylint", "clippy": "lint-linux-x64",
           "build": "build-linux-x64", "unit": "test-linux-x64-unit"}


def _ok_job(jobs: dict[str, object], key: str, steps: tuple[str, ...]) -> bool:
    job = jobs.get(key)
    if (not isinstance(job, dict) or job.get("status") != "completed"
            or job.get("conclusion") != "success"):
        return False
    sections = job.get("sections")
    if not isinstance(sections, list):
        return False
    return all(sum(isinstance(s, dict) and s.get("name") == step and s.get("status") == "completed"
                   and s.get("conclusion") == "success" for s in sections) == 1 for step in steps)


def _parse_job_tree(tree: object) -> dict[str, object]:
    if (not isinstance(tree, dict) or tree.get("malformed_lines") != 0
            or not isinstance(tree.get("groups"), list)):
        raise ValueError("Bosn job tree is missing or malformed")
    jobs: dict[str, object] = {}
    for group in tree["groups"]:
        if not isinstance(group, dict) or not isinstance(group.get("jobs"), list):
            raise ValueError("Bosn job group is malformed")
        for item in group["jobs"]:
            if (not isinstance(item, dict) or not isinstance(item.get("key"), str)
                    or item["key"] in jobs):
                raise ValueError("Bosn job identity is missing or duplicated")
            jobs[item["key"]] = item
    return jobs


def _checked_jobs(result: object, *, head: str, lane: str | None) -> dict[str, object]:
    if not isinstance(result, dict):
        raise ValueError("Bosn returned no structured run record")
    required: dict[str, str | int] = {
        "schema_version": 1, "repository": "zackees/clud", "engine": "act",
        "workflow": ".github/workflows/ci.yml", "trigger": "pr", "mode": "minimal",
        "sha": head, "state": "done", "conclusion": "success",
        "exit_code": 0, "act_exit_code": 0,
    }
    if any(type(result.get(key)) is not type(value) or result.get(key) != value
           for key, value in required.items()):
        raise ValueError("Bosn run metadata differs from this PR head or is not successful")
    if (result.get("dirty") is not None
            or not re.fullmatch(r"[0-9a-f]{64}", str(result.get("tree_digest")))):
        raise ValueError("Bosn did not run a clean source snapshot")
    workspace = result.get("workspace")
    if not isinstance(workspace, str) or Path(workspace).resolve() != ROOT.resolve():
        raise ValueError("Bosn ran a different workspace")
    version = result.get("act_version")
    match = re.fullmatch(r"\d+\.\d+\.\d+-act2\.(\d+)", version) if isinstance(version, str) else None
    if match is None or int(match.group(1)) < 3:
        raise ValueError("Bosn must run act2.3 or later with executed-step proof")
    if result.get("job") != (JOB_IDS[lane] if lane else None):
        raise ValueError("Bosn selected a different job plan")
    return _parse_job_tree(result.get("tree"))


def proved_lanes(result: object, *, head: str, lane: str | None) -> tuple[str, ...]:
    """Return proved lanes, or raise when Bosn's record is incomplete."""
    jobs = _checked_jobs(result, head=head, lane=lane)
    if lane is None and not _ok_job(jobs, "CI/CI OK", ("Check results",)):
        raise ValueError("Bosn full PR gate did not complete CI OK")
    lanes = (lane,) if lane else tuple(LANE_JOBS)
    for named_lane in lanes:
        for key, steps in LANE_JOBS[named_lane]:
            if not _ok_job(jobs, key, steps):
                raise ValueError(f"Bosn did not prove {named_lane}: {key}")
    return lanes


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lane", choices=tuple(LANE_JOBS))
    args = parser.parse_args(argv)
    head = os.environ.get("CI_LINT_GATE_HEAD") or process.check_output(
        ["git", "rev-parse", "HEAD"], cwd=ROOT
    ).strip()
    command = ["bosn", "ci", "run", "--workspace", ".", "--trigger", "pr", "--wait", "--json"]
    if args.lane:
        command.extend(("--job", JOB_IDS[args.lane]))
    started = time.monotonic()
    runner = RunningProcess(command, cwd=ROOT, capture=True, stderr=PIPE)
    lines: list[str] = []
    while (line := runner.get_next_stdout_line()) is not EOS:
        lines.append(str(line))
    returncode = runner.wait()
    try:
        result = json.loads("\n".join(lines))
    except json.JSONDecodeError as exc:
        print(f"local-gate: Bosn returned no parseable run record: {exc}", file=sys.stderr)
        print(str(runner.stderr)[-2000:], file=sys.stderr)
        return returncode or 1
    if not isinstance(result, dict):
        print("local-gate: Bosn returned a non-object run record", file=sys.stderr)
        return returncode or 1
    if returncode != 0:
        print(
            f"local-gate: Bosn run {result.get('id')} failed: {result.get('reason')}",
            file=sys.stderr,
        )
        return returncode
    try:
        lanes = proved_lanes(result, head=head, lane=args.lane)
    except ValueError as exc:
        print(f"local-gate: {exc}; run {result.get('id')}", file=sys.stderr)
        return 1
    elapsed = max(0, round(time.monotonic() - started))
    if not args.lane:
        receipt = os.environ.get("CI_LINT_GATE_RECEIPT")
        tree = os.environ.get("CI_LINT_GATE_TREE")
        if not receipt or not tree or not re.fullmatch(r"[0-9a-f]{40}", tree):
            print("local-gate: missing tree-bound receipt environment", file=sys.stderr)
            return 1
        Path(receipt).write_text(
            json.dumps({
                "version": 1, "tree": tree,
                "passes": [{"lane": name, "secs": elapsed} for name in lanes],
            }),
            encoding="utf-8",
        )
    print(f"local-gate: Bosn run {result.get('id')} proved {', '.join(lanes)} in {elapsed}s")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
