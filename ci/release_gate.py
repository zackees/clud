"""Fail-closed proof that a release candidate passed the complete CI workflow."""

from __future__ import annotations

import json
import os
import re
import sys
from urllib.parse import quote
from urllib.request import Request, urlopen

from ci.ci_matrix import TARGETS

_NAMES = ("linux-x64", "windows-x64", "macos-arm", "linux-arm", "windows-arm", "macos-x64")
assert len(_NAMES) == len(TARGETS)

REQUIRED_JOBS = frozenset(
    {"Static checks", "Dylint / Dylint", "CI OK"}
    | {f"Build {name} / {target.triple}" for name, target in zip(_NAMES, TARGETS, strict=True)}
    | {
        f"Test {name} ({suite}) / {target.triple} {suite}"
        for name, target in zip(_NAMES, TARGETS, strict=True)
        for suite in ("unit", "integration")
    }
)


def verify_run(run: dict, jobs: list[dict], candidate_sha: str) -> None:
    """Reject stale, non-full, incomplete, or skipped execution evidence."""
    if not re.fullmatch(r"[0-9a-f]{40}", candidate_sha):
        raise ValueError("candidate must be an exact lowercase commit SHA")
    if (
        run.get("event") != "workflow_dispatch"
        or run.get("display_title") != f"CI full {candidate_sha}"
    ):
        raise ValueError("CI run is not an exact-candidate full dispatch")
    if run.get("status") != "completed" or run.get("conclusion") != "success":
        raise ValueError("full CI run has not succeeded")
    by_name: dict[str, list[dict]] = {}
    for job in jobs:
        by_name.setdefault(job.get("name", ""), []).append(job)
    missing = sorted(REQUIRED_JOBS - by_name.keys())
    if missing:
        raise ValueError(f"missing full CI execution cells: {', '.join(missing)}")
    bad = sorted(
        name for name in REQUIRED_JOBS
        if len(by_name[name]) != 1
        or by_name[name][0].get("status") != "completed"
        or by_name[name][0].get("conclusion") != "success"
    )
    if bad:
        raise ValueError(f"full CI cells did not succeed: {', '.join(bad)}")


def api(path: str) -> dict:
    token = os.environ["GH_TOKEN"]
    request = Request(
        f"https://api.github.com/{path}",
        headers={"Authorization": f"Bearer {token}", "Accept": "application/vnd.github+json"},
    )
    with urlopen(request, timeout=30) as response:
        return json.load(response)


def verify_candidate(repo: str, candidate_sha: str) -> str:
    if not re.fullmatch(r"[0-9a-f]{40}", candidate_sha):
        raise ValueError("candidate must be an exact lowercase commit SHA")
    repo_path = quote(repo, safe="/")
    # The run title is bound to candidate_sha by ci.yml; CI's source check
    # verifies every reusable job receives that SHA. Scan recent dispatches so
    # a failed retry does not erase an earlier successful proof on the same SHA.
    runs_path = (
        f"repos/{repo_path}/actions/workflows/ci.yml/runs"
        "?event=workflow_dispatch&per_page=100"
    )
    runs = api(runs_path)["workflow_runs"]
    for run in runs:
        if run.get("display_title") != f"CI full {candidate_sha}":
            continue
        if run.get("status") != "completed" or run.get("conclusion") != "success":
            continue
        run_id = run["id"]
        jobs: list[dict] = []
        for page in range(1, 10):
            jobs_path = f"repos/{repo_path}/actions/runs/{run_id}/jobs?per_page=100&page={page}"
            batch = api(jobs_path)["jobs"]
            jobs.extend(batch)
            if len(batch) < 100:
                break
        else:
            raise ValueError("full CI job listing exceeded pagination limit")
        try:
            verify_run(run, jobs, candidate_sha)
        except ValueError:
            continue
        return run["html_url"]
    raise ValueError(f"no successful, complete exact-SHA full CI run for {candidate_sha}")


def main() -> int:
    repo = os.environ["GITHUB_REPOSITORY"]
    sha = os.environ["CANDIDATE_SHA"]
    try:
        url = verify_candidate(repo, sha)
    except (KeyError, ValueError) as error:
        print(f"::error::{error}", file=sys.stderr)
        return 1
    print(f"Full CI proof: {url}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
