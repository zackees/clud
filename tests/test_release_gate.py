"""The release gate requires executed full CI on the exact candidate."""

from __future__ import annotations

from pathlib import Path

import pytest

from ci import release_gate
from ci.release_gate import REQUIRED_JOBS, verify_run

WORKFLOWS = Path(__file__).resolve().parent.parent / ".github" / "workflows"


def full_run(sha: str) -> dict:
    return {
        "event": "workflow_dispatch",
        "status": "completed",
        "conclusion": "success",
        "display_title": f"CI full {sha}",
    }


def full_jobs() -> list[dict]:
    return [
        {"name": name, "conclusion": "success", "status": "completed"}
        for name in REQUIRED_JOBS
    ]


def test_full_ci_success_requires_every_executed_cell() -> None:
    sha = "a" * 40
    verify_run(full_run(sha), full_jobs(), sha)
    for missing in REQUIRED_JOBS:
        with pytest.raises(ValueError, match="missing"):
            verify_run(full_run(sha), [job for job in full_jobs() if job["name"] != missing], sha)


@pytest.mark.parametrize("change", [
    {"display_title": "CI full " + "b" * 40},
    {"event": "push"},
    {"status": "in_progress"},
    {"conclusion": "failure"},
])
def test_wrong_or_incomplete_run_fails(change: dict) -> None:
    sha = "a" * 40
    with pytest.raises(ValueError, match=r"full dispatch|not succeeded"):
        verify_run({**full_run(sha), **change}, full_jobs(), sha)


@pytest.mark.parametrize("state", ["skipped", "failure", "cancelled", None])
def test_required_job_must_succeed(state: str | None) -> None:
    sha = "a" * 40
    jobs = full_jobs()
    jobs[-1]["conclusion"] = state
    with pytest.raises(ValueError, match="did not succeed"):
        verify_run(full_run(sha), jobs, sha)


def test_release_workflow_gates_every_publish_path() -> None:
    text = (WORKFLOWS / "auto-release.yml").read_text(encoding="utf-8")
    assert "candidate_sha: ${{ steps.meta.outputs.candidate_sha }}" in text
    assert "run: python -m ci.release_gate" in text
    assert "needs: [preflight, full-ci-gate, release-matrix]" in text
    assert "needs: [preflight, full-ci-gate, build]" in text
    assert "needs: [preflight, full-ci-gate, build, publish-pypi]" in text
    assert "source_ref: ${{ needs.preflight.outputs.candidate_sha }}" in text
    assert "ref: ${{ needs.preflight.outputs.candidate_sha }}" in text
    assert "branches: [main]" not in text  # A main version bump cannot start a release.


def test_candidate_proof_is_bound_to_dispatch_title() -> None:
    text = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
    assert "format('CI full {0}', inputs.candidate_sha)" in text


def test_api_lookup_requires_a_complete_matching_run(monkeypatch) -> None:
    sha = "a" * 40
    unrelated = {**full_run("b" * 40), "id": 1}
    matching = {**full_run(sha), "id": 2, "html_url": "https://github.com/o/r/actions/runs/2"}

    def fake_api(path: str) -> dict:
        if path.endswith("/runs?event=workflow_dispatch&per_page=100"):
            return {"workflow_runs": [unrelated, matching]}
        if "/runs/2/jobs?" in path:
            return {"jobs": full_jobs()}
        raise AssertionError(path)

    monkeypatch.setattr(release_gate, "api", fake_api)
    assert release_gate.verify_candidate("o/r", sha) == matching["html_url"]

    monkeypatch.setattr(release_gate, "api", lambda _path: {"workflow_runs": [unrelated]})
    with pytest.raises(ValueError, match="no successful"):
        release_gate.verify_candidate("o/r", sha)
