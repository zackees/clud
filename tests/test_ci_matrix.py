"""Unit tests for the CI target matrix.

The matrix is the contract between ci/ci_matrix.py and the `strategy: matrix:`
blocks in .github/workflows/ci.yml and auto-release.yml. A typo here is only
discoverable by pushing to CI, which is exactly the slow feedback loop the
redesign exists to eliminate -- so it is tested here instead.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

import pytest

from ci.ci_matrix import (
    SDIST_TARGET,
    SUITES,
    TARGETS,
    build_matrix,
    exec_matrix,
    release_matrix,
    resolve_tier,
    selected,
)

CI_YML = Path(__file__).resolve().parent.parent / ".github" / "workflows" / "ci.yml"


def test_every_target_is_unique():
    triples = [target.triple for target in TARGETS]
    assert len(triples) == len(set(triples))


def test_core_tier_covers_every_operating_system():
    """core must exercise Linux, Windows and macOS.

    The platform-gated tests are `#![cfg(windows)]` / `#![cfg(unix)]`, not
    arch-gated, so one triple per OS is what makes the reduced tier safe.
    """
    core = selected("core")
    families = {triple.split("-")[2] for triple in (target.triple for target in core)}
    assert families == {"linux", "windows", "darwin"}


def test_full_tier_is_a_superset_of_core():
    core = {target.triple for target in selected("core")}
    full = {target.triple for target in selected("full")}
    assert core < full
    assert full == {target.triple for target in TARGETS}


@pytest.mark.parametrize(
    ("event", "dispatch", "labels", "expected"),
    [
        ("pull_request", "", "", "core"),
        ("pull_request", "", "ci:full", "full"),
        ("pull_request", "", "documentation,ci:full,bug", "full"),
        ("pull_request", "", "ci:full-ish", "core"),
        # main pushes and the merge queue always run full: that is what keeps
        # every triple's build cache warm for PR jobs to restore from.
        ("push", "", "", "full"),
        ("merge_group", "", "", "full"),
        ("workflow_dispatch", "core", "", "core"),
        ("workflow_dispatch", "full", "", "full"),
    ],
)
def test_resolve_tier(event, dispatch, labels, expected):
    assert resolve_tier(event, dispatch, labels) == expected


def test_every_target_cross_compiles_on_linux():
    """No native-builder fallback survives: one build host for all six triples.

    That is the whole point of build-once/run-everywhere -- a single triple
    escaping to its own native runner reintroduces the cold C++ build the
    redesign exists to delete.
    """
    include = build_matrix(selected("full"))["include"]
    assert len(include) == len(TARGETS)
    assert all(entry["runs-on"] == "ubuntu-24.04" for entry in include)


def test_darwin_and_windows_use_the_soldr_blessed_cross_path():
    """`cargo xwin` / `cargo zigbuild --target *-apple-darwin` are the legacy
    passthrough in soldr's docs/CROSS_COMPILE.md; `soldr build` is the blessed
    surface. Pinning this here keeps a future edit from silently regressing to
    a hand-installed cross wrapper."""
    include = build_matrix(selected("full"))["include"]
    crossed = [
        entry
        for entry in include
        if "apple" in entry["target"] or "windows" in entry["target"]
    ]
    assert len(crossed) == 4
    assert all(entry["strategy"] == "soldr" for entry in crossed)


def test_no_target_uses_a_hand_installed_cross_wrapper_for_msvc_or_darwin():
    for target in TARGETS:
        if "windows" in target.triple or "apple" in target.triple:
            assert target.strategy == "soldr", target.triple


def test_test_matrix_always_uses_native_runners():
    include = exec_matrix(selected("full"))["include"]
    assert len(include) == len(TARGETS) * len(SUITES)
    expected = {target.triple: target.exec_runs_on for target in TARGETS}
    for entry in include:
        assert entry["runs-on"] == expected[entry["target"]]
        assert entry["suite"] in SUITES


def test_release_matrix_ships_exactly_one_sdist():
    include = release_matrix()["include"]
    sdists = [entry for entry in include if entry["include-sdist"]]
    assert len(sdists) == 1
    assert sdists[0]["target"] == SDIST_TARGET


def test_release_matrix_artifact_names_are_unique_and_complete():
    include = release_matrix()["include"]
    artifacts = [entry["artifact"] for entry in include]
    assert len(artifacts) == len(TARGETS) == len(set(artifacts))
    assert all(name.startswith("wheels-") for name in artifacts)


def test_release_matrix_uses_the_same_strategies_as_ci():
    """Release must ship what CI tested.

    If release built natively while CI only exercised cross-built binaries, CI
    would never have validated the artifact that ships.
    """
    def strategies(matrix):
        return {entry["target"]: entry["strategy"] for entry in matrix["include"]}

    assert strategies(build_matrix(selected("full"))) == strategies(release_matrix())


def test_ci_yml_covers_exactly_the_targets_table():
    """ci.yml spells out one build/test job pair per triple; keep them in sync.

    The pairs cannot be a matrix: `needs:` on a matrix job is all-or-nothing in
    GitHub Actions, so a single test matrix would make the fast Linux lane wait
    on the slowest cross-build. The cost of that workaround is hand-written
    YAML, and the cost of hand-written YAML is drift -- which is what this test
    exists to prevent.
    """
    text = CI_YML.read_text(encoding="utf-8")
    declared = set(re.findall(r"^      target: (\S+)$", text, re.MULTILINE))
    assert declared == {target.triple for target in TARGETS}

    # Every build job must have a matching test job that depends on it, or the
    # triple gets compiled and then never exercised.
    build_jobs = set(re.findall(r"^  build-([a-z0-9-]+):$", text, re.MULTILINE))
    test_needs = set(re.findall(r"^    needs: build-([a-z0-9-]+)$", text, re.MULTILINE))
    assert build_jobs == test_needs
    assert len(build_jobs) == len(TARGETS)


def test_ci_yml_job_configuration_matches_the_targets_table():
    """The hand-written jobs must preserve every registry field, not just names."""
    text = CI_YML.read_text(encoding="utf-8")
    expected = {target.triple: target for target in TARGETS}

    build_blocks = re.findall(
        r"^  build-[a-z0-9-]+:\n(.*?)(?=^  [a-z0-9-]+:|\Z)",
        text,
        re.MULTILINE | re.DOTALL,
    )
    declared_builds = {}
    for block in build_blocks:
        triple = re.search(r"^      target: (\S+)$", block, re.MULTILINE)
        runner = re.search(r"^      runs-on: (\S+)$", block, re.MULTILINE)
        strategy = re.search(r"^      strategy: (\S+)$", block, re.MULTILINE)
        assert triple
        assert runner
        assert strategy
        declared_builds[triple.group(1)] = (runner.group(1), strategy.group(1))

    assert declared_builds == {
        triple: (target.build_runs_on, target.strategy)
        for triple, target in expected.items()
    }

    test_blocks = re.findall(
        r"^  test-[a-z0-9-]+:\n(.*?)(?=^  [a-z0-9-]+:|\Z)",
        text,
        re.MULTILINE | re.DOTALL,
    )
    declared_exec = {}
    for block in test_blocks:
        triple = re.search(r"^      target: (\S+)$", block, re.MULTILINE)
        runner = re.search(r"^      runs-on: (\S+)$", block, re.MULTILINE)
        assert triple
        assert runner
        declared_exec[triple.group(1)] = runner.group(1)

    assert declared_exec == {
        triple: target.exec_runs_on for triple, target in expected.items()
    }


def test_ci_yml_never_requests_a_release_profile():
    """Requirement: only the release pipeline builds --release.

    _build-target.yml also guards this at run time on `github.workflow`, but a
    static check fails in `bash test` rather than after a runner has spun up.
    """
    assert "profile: release" not in CI_YML.read_text(encoding="utf-8")


def test_matrices_are_json_serializable_for_github_actions():
    """`fromJSON()` in the workflow needs valid, single-line JSON."""
    for matrix in (
        build_matrix(selected("full")),
        exec_matrix(selected("core")),
        release_matrix(),
    ):
        encoded = json.dumps(matrix, separators=(",", ":"))
        assert "\n" not in encoded
        assert json.loads(encoded) == matrix
