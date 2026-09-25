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
BUILD_YML = CI_YML.with_name("_build-target.yml")


@pytest.mark.parametrize(
    "path",
    [
        BUILD_YML,
        CI_YML.with_name("rm-protection-docker.yml"),
        CI_YML.parent.parent.parent / "bosn.toml",
    ],
)
def test_builds_leave_compile_concurrency_to_soldr(path: Path) -> None:
    text = path.read_text(encoding="utf-8")
    assert not re.search(r"^\s*(?:SOLDR_JOBS|CARGO_BUILD_JOBS)\s*[:=]", text, re.MULTILINE)


def test_every_target_is_unique():
    triples = [target.triple for target in TARGETS]
    assert len(triples) == len(set(triples))


def test_core_tier_avoids_hosted_macos():
    """The ci-test tier may add Windows, but macOS is reserved for full CI."""
    core = selected("core")
    families = {triple.split("-")[2] for triple in (target.triple for target in core)}
    assert families == {"linux", "windows"}


def test_full_and_release_include_both_hosted_macos_architectures():
    full = {target.triple for target in selected("full")}
    assert {"aarch64-apple-darwin", "x86_64-apple-darwin"} <= full
    release = {entry["target"] for entry in release_matrix()["include"]}
    assert {"aarch64-apple-darwin", "x86_64-apple-darwin"} <= release
    workflow = CI_YML.read_text(encoding="utf-8")
    arm = workflow.split("\n  build-macos-arm:\n", 1)[1].split("\n  test-macos-arm:\n", 1)[0]
    assert "if: needs.static.outputs.mode == 'full'" in arm
    gate = workflow.split("\n  ci-ok:\n", 1)[1]
    extended = gate.split("EXTENDED: >-", 1)[1].split("FULL: >-", 1)[0]
    full_gate = gate.split("FULL: >-", 1)[1]
    assert "needs.test-macos-arm.result" not in extended
    assert "needs.test-macos-arm.result" in full_gate


def test_full_tier_is_a_superset_of_core():
    core = {target.triple for target in selected("core")}
    full = {target.triple for target in selected("full")}
    assert core < full
    assert full == {target.triple for target in TARGETS}


def test_minimal_and_extended_modes_are_strict_subsets():
    minimal = {target.triple for target in selected("minimal")}
    extended = {target.triple for target in selected("extended")}
    full = {target.triple for target in selected("full")}
    assert minimal == {"x86_64-unknown-linux-gnu"}
    assert minimal < extended < full


@pytest.mark.parametrize(
    ("event", "dispatch", "labels", "expected"),
    [
        ("pull_request", "", "", "minimal"),
        ("pull_request", "", "ci-test", "extended"),
        ("pull_request", "", "ci-full", "full"),
        ("pull_request", "", "ci-test,ci-full", "full"),
        ("pull_request", "", "ci:full", "full"),
        ("push", "", "", "minimal"),
        ("merge_group", "", "", "full"),
    ],
)
def test_resolve_tier(event, dispatch, labels, expected):
    assert resolve_tier(event, dispatch, labels) == expected


def test_full_dispatch_requires_exact_candidate_sha():
    assert resolve_tier("workflow_dispatch", "full", "", "a" * 40, "a" * 40, True) == "full"
    assert resolve_tier("workflow_dispatch", "full", "", "a" * 40, "b" * 40, True) == "full"
    with pytest.raises(ValueError, match="provenance"):
        resolve_tier("workflow_dispatch", "full", "", "a" * 40, "b" * 40, False)
    with pytest.raises(ValueError, match="candidate"):
        resolve_tier("workflow_dispatch", "full", "")


def test_pr_and_dispatch_source_ref_is_pinned_in_every_job():
    text = CI_YML.read_text(encoding="utf-8")
    assert "github.event.pull_request.head.sha" in text
    assert "git merge-base --is-ancestor" in text
    assert "git diff --quiet" in text
    for name in ("_build-target.yml", "_run-tests.yml", "_dylint.yml"):
        reusable = CI_YML.with_name(name).read_text(encoding="utf-8")
        assert "source_ref:" in reusable, name
        assert "ref: ${{ inputs.source_ref || github.sha }}" in reusable, name
    assert text.count("source_ref: ${{ needs.static.outputs.source_ref }}") == 14


def test_each_target_executes_both_test_suites_and_gate_checks_every_target():
    text = CI_YML.read_text(encoding="utf-8")
    gate = text.split("\n  ci-ok:\n", 1)[1]
    for name in ("windows-x64", "macos-arm", "linux-arm", "windows-arm", "macos-x64"):
        match = re.search(
            rf"^  test-{name}:\n(.*?)(?=^  [a-z0-9-]+:|\Z)",
            text,
            re.MULTILINE | re.DOTALL,
        )
        assert match, name
        block = match.group(1)
        assert "suite: [unit, integration]" in block, name
        assert f"${{{{ needs.test-{name}.result }}}}" in gate, name

    unit = text.split("\n  test-linux-x64-unit:\n", 1)[1].split(
        "\n  test-linux-x64-integration:\n", 1
    )[0]
    integration = text.split("\n  test-linux-x64-integration:\n", 1)[1].split(
        "\n  build-windows-x64:\n", 1
    )[0]
    assert "suite: unit" in unit
    assert "suite: integration" in integration
    assert "if: needs.static.outputs.mode != 'minimal'" in integration
    assert "${{ needs.test-linux-x64-unit.result }}" in gate
    assert "${{ needs.test-linux-x64-integration.result }}" in gate
    minimal = gate.split("MINIMAL: >-", 1)[1].split("EXTENDED: >-", 1)[0]
    extended = gate.split("EXTENDED: >-", 1)[1].split("FULL: >-", 1)[0]
    assert "needs.test-linux-x64-unit.result" in minimal
    assert "needs.test-linux-x64-integration.result" not in minimal
    assert "needs.test-linux-x64-integration.result" in extended


def test_unknown_ci_label_fails_closed():
    with pytest.raises(ValueError, match="unknown"):
        resolve_tier("pull_request", "", "ci-ful")


def test_workflow_binds_dispatch_and_labels_to_mode():
    text = CI_YML.read_text(encoding="utf-8")
    assert "types: [opened, synchronize, reopened, labeled, unlabeled]" in text
    assert "candidate_sha:" in text
    assert "CANDIDATE_SHA: ${{ inputs.candidate_sha }}" in text
    assert "EVENT_SHA: ${{ github.sha }}" in text
    assert "run: python -m ci.ci_matrix" in text
    assert "MODE: ${{ needs.static.outputs.mode }}" in text
    assert 'case "$MODE" in minimal|extended|full|windows)' in text


def test_every_build_waits_for_mode_and_full_is_complete():
    text = CI_YML.read_text(encoding="utf-8")
    for name in ("linux-x64", "windows-x64", "macos-arm", "linux-arm", "windows-arm", "macos-x64"):
        block = text.split(f"\n  build-{name}:\n", 1)[1].split("\n  test-", 1)[0]
        assert "    needs: static\n" in block, name
        assert "    if: needs.static.outputs.mode" in block, name
    assert "${{ needs.dylint.result }}" in text
    assert 'if [ "$MODE" = "full" ]; then' in text


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
    """The legacy passthrough in soldr's docs/CROSS_COMPILE.md is
    `cargo xwin` / `cargo zigbuild`; (cross-lint: allow — named on purpose)
    `soldr build` is the blessed surface. Pinning this here keeps a future
    edit from silently regressing to a hand-installed cross wrapper."""
    include = build_matrix(selected("full"))["include"]
    crossed = [
        entry for entry in include if "apple" in entry["target"] or "windows" in entry["target"]
    ]
    assert len(crossed) == 4
    assert all(entry["strategy"] == "soldr" for entry in crossed)


def test_no_target_uses_a_hand_installed_cross_wrapper_for_msvc_or_darwin():
    for target in TARGETS:
        if "windows" in target.triple or "apple" in target.triple:
            assert target.strategy == "soldr", target.triple


def test_cross_argv_never_routes_apple_or_msvc_through_a_banned_tool():
    """The behavioural half of #637, and the half a text scan cannot do.

    `ci/banned_cross_tools.py` catches a literal command with a literal target.
    It cannot follow a target held in a variable — and `cargo_argv` takes the
    target as an argument, so every real invocation in this repo is exactly the
    shape the text scan is blind to.

    So ask the real function. For every triple in the matrix, with every
    strategy it could plausibly be given, assert the argv it produces names no
    banned wrapper when the target is Apple or MSVC. A future edit that routes
    a darwin build through zigbuild fails here even though no file contains the
    string `zigbuild --target aarch64-apple-darwin`.
    """
    from ci.xbuild import cargo_argv

    banned = ("xwin", "zigbuild", "zig", "cross", "osxcross")  # cross-lint: allow
    soldr_owned = [t for t in TARGETS if "apple" in t.triple or "windows" in t.triple]
    assert soldr_owned, "matrix has no crossed targets; this test would be vacuous"

    for target in soldr_owned:
        # Not just the strategy the matrix assigns today: a future edit could
        # change it, and the point is that no strategy may reach a banned tool
        # for these triples.
        for strategy in ("native", "zigbuild", "soldr"):
            for subcommand in (["build"], ["test", "--no-run"], ["clippy"]):
                if strategy == "zigbuild":
                    # Refused outright rather than silently producing a working
                    # zigbuild argv. Returning something safe would let the
                    # misconfiguration sit in the matrix unnoticed.
                    with pytest.raises(ValueError, match="soldr"):
                        cargo_argv(subcommand, target.triple, strategy)
                    continue
                argv = cargo_argv(subcommand, target.triple, strategy)
                assert not any(token in banned for token in argv), (
                    f"{target.triple} / {strategy} / {subcommand[0]} produced "
                    f"{argv}, which drives a banned cross tool"
                )


def test_gnu_linux_is_now_soldr_owned_and_refuses_zig():
    """soldr#2299 reversal: soldr 0.8.39's catalogue GNU toolchain replaced zig
    for *-unknown-linux-gnu, so routing a GNU/Linux build through zigbuild is
    refused, exactly as for Apple/MSVC. (Was the zig-stays-for-Linux counter-
    weight to #637.)"""
    from ci.xbuild import cargo_argv, is_soldr_owned

    assert is_soldr_owned("x86_64-unknown-linux-gnu")
    assert is_soldr_owned("aarch64-unknown-linux-gnu")
    with pytest.raises(ValueError, match="soldr"):
        cargo_argv(["build"], "aarch64-unknown-linux-gnu", "zigbuild")
    # The blessed surface is what a GNU/Linux build uses now.
    assert cargo_argv(["build"], "x86_64-unknown-linux-gnu", "soldr")[:2] == ["soldr", "build"]


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


def test_doc_tests_run_on_a_live_matrix_triple():
    """#863: doc-tests were silently disabled when #859 retired the `native`
    strategy — the step's `if:` keyed on `strategy == 'native'`, which no lane
    passes, and it is the ONLY doc-test runner in CI (they produce no harness
    binary, so they cannot ride in the exec bundle).

    Pin the key to something the matrix can actually satisfy: the condition
    must name a target triple, that triple must exist in TARGETS, and no
    strategy comparison may guard the step.
    """
    build_target = CI_YML.parent / "_build-target.yml"
    text = build_target.read_text(encoding="utf-8")
    step = re.search(r"- name: Doc tests\n\s+if: \$\{\{ (.+?) \}\}", text, re.DOTALL)
    assert step, "_build-target.yml has no conditional Doc tests step"
    condition = step.group(1)

    assert "strategy" not in condition, (
        "doc-tests must not key on the strategy — that is how #863 silently "
        f"disabled them: {condition}"
    )
    named = re.search(r"inputs\.target == '([^']+)'", condition)
    assert named, f"doc-test condition must pin a target triple: {condition}"
    live = {target.triple for target in TARGETS}
    assert named.group(1) in live, (
        f"doc-tests keyed on {named.group(1)}, which is not in the matrix — they would never run"
    )


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
    test_needs = set(re.findall(r"^    needs: \[static, build-([a-z0-9-]+)\]$", text, re.MULTILINE))
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
        triple: (target.build_runs_on, target.strategy) for triple, target in expected.items()
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

    assert declared_exec == {triple: target.exec_runs_on for triple, target in expected.items()}


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


def test_ci_windows_label_selects_only_windows_x64():
    """#1310: an iteration-only mode for Windows work."""
    assert resolve_tier("pull_request", "", "ci-windows") == "windows"
    assert [t.triple for t in selected("windows")] == ["x86_64-pc-windows-msvc"]
    # Merge-gating tiers win when both labels are present.
    assert resolve_tier("pull_request", "", "ci-windows,ci-test") == "extended"
    assert resolve_tier("pull_request", "", "ci-windows,ci-full") == "full"


def test_ci_windows_mode_skips_linux_and_gates_on_windows_lanes():
    text = CI_YML.read_text(encoding="utf-8")
    linux = text.split("\n  build-linux-x64:\n", 1)[1].split("\n\n", 1)[0]
    assert "needs.static.outputs.mode != 'windows'" in linux
    integration = text.split("\n  test-linux-x64-integration:\n", 1)[1].split("\n\n", 1)[0]
    assert "needs.static.outputs.mode != 'windows'" in integration
    windows = text.split("\n  build-windows-x64:\n", 1)[1].split("\n\n", 1)[0]
    assert "needs.static.outputs.mode == 'windows'" in windows
    gate = text.split("\n  ci-ok:\n", 1)[1]
    branch = gate.split('if [ "$MODE" = "windows" ]; then', 1)[1].split("\n          fi\n", 1)[0]
    # Any failed static/Windows lane fails the gate; otherwise the mode passes.
    assert "for result in $STATIC $WINDOWS; do" in branch
    assert branch.rstrip().endswith("exit 0")
