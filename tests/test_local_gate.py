"""The local gate's wiring (zackees/ci.yml GATE-001..011; local-gate.toml).

The ci_lint pin must agree everywhere it is written, the remote `static`
job must run only the gate's `static` lane, and the `ci` lane must refuse a
bosn run that did not test this worktree's HEAD or skipped a minimal job.
"""

from __future__ import annotations

import re
from pathlib import Path

from ci import local_gate

ROOT = Path(__file__).resolve().parents[1]
CI_YML = ROOT / ".github" / "workflows" / "ci.yml"


def test_ci_lint_pin_is_one_sha_everywhere() -> None:
    pins = {
        "ci/local_gate.py": {local_gate.CI_LINT_REF},
        "ci.yml": set(re.findall(r"zackees/ci\.yml@([0-9a-f]{40})", CI_YML.read_text("utf-8"))),
        "local-gate.toml": set(
            re.findall(r"zackees/ci\.yml@([0-9a-f]{40})", (ROOT / "local-gate.toml").read_text("utf-8"))
        ),
    }
    assert all(found == {local_gate.CI_LINT_REF} for found in pins.values()), pins


def test_static_job_runs_only_the_static_lane() -> None:
    text = CI_YML.read_text("utf-8")
    static = text.split("\n  static:\n", 1)[1].split("\n  dylint:\n", 1)[0]
    assert "run: .venv/bin/python ci/local_gate.py --lane static" in static
    assert "-m ci.lint" not in static
    assert "ci-lint local-gate verify --repo . --trust --github-output" in static


def test_every_skip_job_consumes_its_own_output() -> None:
    text = CI_YML.read_text("utf-8")
    for job in ("dylint", "lint-linux-x64", "build-linux-x64", "test-linux-x64-unit"):
        block = text.split(f"\n  {job}:\n", 1)[1].split("\n\n", 1)[0]
        assert f"needs.static.outputs.skip_{job} != 'true'" in block, job
        assert f"skip_{job}: ${{{{ steps.gate.outputs.skip_{job} }}}}" in text, job


def _receipt(**overrides: object) -> local_gate.Receipt:
    jobs = (
        ("CI/Static checks", "success"),
        ("Dylint/Dylint/Dylint", "success"),
        ("Clippy linux-x64/Build target/x86_64-unknown-linux-gnu", "success"),
        ("Build linux-x64/Build target/x86_64-unknown-linux-gnu", "success"),
        ("Test linux-x64 (unit)-1/Run tests/x86_64-unknown-linux-gnu unit", "success"),
        ("Test linux-x64 (unit)-2/Run tests/x86_64-unknown-linux-gnu unit", "success"),
        ("Test linux-x64 (unit)-3/Run tests/x86_64-unknown-linux-gnu unit", "success"),
        ("build-windows-x64", "skipped"),
        ("CI/CI OK", "success"),
    )
    fields: dict[str, object] = {
        "run_id": "r",
        "workspace": str(local_gate.ROOT),
        "sha": "a" * 40,
        "dirty": False,
        "state": "done",
        "conclusion": "success",
        "jobs": jobs,
    }
    fields.update(overrides)
    return local_gate.Receipt(**fields)  # type: ignore[arg-type]


def test_a_clean_full_run_of_head_passes() -> None:
    receipt = _receipt()
    assert local_gate._tree_problem(receipt, "a" * 40) is None
    assert local_gate._job_problems(receipt) == []


def test_tree_proof_refuses_another_tree() -> None:
    assert local_gate._tree_problem(_receipt(sha="b" * 40), "a" * 40)
    assert local_gate._tree_problem(_receipt(dirty=True), "a" * 40)
    assert local_gate._tree_problem(_receipt(workspace="/elsewhere"), "a" * 40)


def test_a_missing_or_failed_minimal_job_fails_the_lane() -> None:
    receipt = _receipt()
    without_shard = tuple(j for j in receipt.jobs if not j[0].startswith("Test linux-x64 (unit)-3"))
    assert local_gate._job_problems(_receipt(jobs=without_shard))
    failed = tuple((k, "failure" if k.startswith("Dylint/") else c) for k, c in receipt.jobs)
    assert local_gate._job_problems(_receipt(jobs=failed))
