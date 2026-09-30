"""The Linux x64 unit suite is split across independent runners, by file.

It is CPU-bound and a hosted runner is two physical cores, so pytest-xdist
measured slower (172 s vs ~135 s serial). Separate machines do not contend:
one job runs the Rust harnesses and two jobs run half of the pytest suite each.

The property that matters is coverage: the shards must be disjoint and together
run every test, with weights that are only a balance hint. The release gate
must then require exactly the jobs the workflow produces, or a release could
pass with a shard unproven.
"""

from __future__ import annotations

import json
import re
from pathlib import Path
from types import SimpleNamespace

import pytest

from ci import pytest_shard, release_gate, run_bundle
from ci.ci_matrix import LINUX_X64_UNIT_SHARDS, TARGETS
from ci.pytest_shard import assign_files, load_weights, parse_spec
from ci.run_bundle import Shard, parse_shard, run_unit_suite

ROOT = Path(__file__).resolve().parent.parent
CI_YML = ROOT / ".github" / "workflows" / "ci.yml"
RUN_TESTS = ROOT / ".github" / "workflows" / "_run-tests.yml"


# ------------------------------------------------------------------ parsing --


@pytest.mark.parametrize(
    ("name", "expected"),
    [
        ("all", Shard(rust=True, python=True)),
        ("rust", Shard(rust=True, python=False)),
        ("py1of2", Shard(rust=False, python=True, spec="1/2")),
        ("py2of2", Shard(rust=False, python=True, spec="2/2")),
    ],
)
def test_parse_shard(name: str, expected: Shard) -> None:
    assert parse_shard(name) == expected


@pytest.mark.parametrize("name", ["", "py", "py0of2", "py3of2", "py1of0", "rust2", "ALL"])
def test_unknown_shard_names_fail_closed(name: str) -> None:
    with pytest.raises(ValueError, match="unknown unit shard"):
        parse_shard(name)


@pytest.mark.parametrize(("spec", "expected"), [("1/2", (0, 2)), ("2/2", (1, 2)), ("3/3", (2, 3))])
def test_parse_spec(spec: str, expected: tuple[int, int]) -> None:
    assert parse_spec(spec) == expected


@pytest.mark.parametrize("spec", ["", "1", "0/2", "3/2", "1/0", "a/b", "1/2/3"])
def test_bad_shard_spec_fails_closed(spec: str) -> None:
    with pytest.raises(ValueError, match=r"must look like|need 1 <= k <= n"):
        parse_spec(spec)


# --------------------------------------------------------------- assignment --


def _shards(counts: dict[str, int], weights: dict[str, float], n: int) -> list[set[str]]:
    assignment = assign_files(counts, weights, n)
    return [{name for name, shard in assignment.items() if shard == k} for k in range(n)]


@pytest.mark.parametrize("n", [1, 2, 3, 5])
def test_shards_are_disjoint_and_cover_every_file(n: int) -> None:
    counts = {f"tests/test_{i}.py": (i % 7) + 1 for i in range(41)}
    weights = {name: float(i % 5) for i, name in enumerate(counts) if i % 3}
    shards = _shards(counts, weights, n)
    assert set().union(*shards) == set(counts)
    assert sum(len(shard) for shard in shards) == len(counts)


def test_assignment_is_deterministic_and_order_independent() -> None:
    counts = {f"tests/test_{i}.py": i + 1 for i in range(20)}
    weights = {f"tests/test_{i}.py": float(i) for i in range(0, 20, 2)}
    first = assign_files(counts, weights, 3)
    shuffled = dict(reversed(list(counts.items())))
    assert assign_files(shuffled, weights, 3) == first


def test_unrecorded_files_are_weighted_by_test_count() -> None:
    counts = {"tests/test_big.py": 500, "tests/test_a.py": 1, "tests/test_b.py": 1}
    shards = _shards(counts, {}, 2)
    big = next(shard for shard in shards if "tests/test_big.py" in shard)
    assert big == {"tests/test_big.py"}  # the rest balance against it


def test_recorded_weights_beat_test_counts() -> None:
    counts = {"tests/test_slow.py": 1, "tests/test_fast1.py": 50, "tests/test_fast2.py": 50}
    weights = {"tests/test_slow.py": 100.0, "tests/test_fast1.py": 1.0, "tests/test_fast2.py": 1.0}
    shards = _shards(counts, weights, 2)
    slow = next(shard for shard in shards if "tests/test_slow.py" in shard)
    assert slow == {"tests/test_slow.py"}


def test_checked_in_weights_split_the_suite_into_balanced_halves() -> None:
    weights = load_weights()
    assert weights, "ci/unit_shard_weights.json is missing or unreadable"
    counts = dict.fromkeys(weights, 1)
    for n in (2, 3):
        loads = [0.0] * n
        for name, shard in assign_files(counts, weights, n).items():
            loads[shard] += weights[name]
        assert max(loads) / (sum(loads) / n) < 1.1, loads


def test_weights_file_is_plain_seconds_per_test_file() -> None:
    raw = json.loads(pytest_shard.WEIGHTS_PATH.read_text(encoding="utf-8"))
    assert raw
    for name, seconds in raw.items():
        assert re.fullmatch(r"tests/.+\.py", name), name
        assert isinstance(seconds, (int, float)), name
        assert seconds >= 0, name


def test_missing_weights_file_only_costs_balance(tmp_path: Path) -> None:
    assert load_weights(tmp_path / "absent.json") == {}
    bad = tmp_path / "bad.json"
    bad.write_text("{not json", encoding="utf-8")
    assert load_weights(bad) == {}


# ------------------------------------------------------------------- plugin --


class _Hook:
    def __init__(self) -> None:
        self.deselected: list[object] = []

    def pytest_deselected(self, items: list[object]) -> None:
        self.deselected.extend(items)


def _items(files: dict[str, int]) -> list[SimpleNamespace]:
    return [
        SimpleNamespace(nodeid=f"{name}::test_{i}")
        for name, count in files.items()
        for i in range(count)
    ]


def test_plugin_keeps_only_this_shards_files_and_reports_the_rest(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    files = {f"tests/test_{i}.py": (i % 4) + 1 for i in range(12)}
    monkeypatch.setattr(pytest_shard, "load_weights", lambda: {})
    kept: list[set[str]] = []
    for spec in ("1/2", "2/2"):
        monkeypatch.setenv(pytest_shard.SHARD_ENV, spec)
        items = _items(files)
        hook = _Hook()
        config = SimpleNamespace(hook=hook)
        pytest_shard.pytest_collection_modifyitems(config, items)  # type: ignore[arg-type]
        kept.append({item.nodeid for item in items})
        assert len(items) + len(hook.deselected) == sum(files.values())
    every = {item.nodeid for item in _items(files)}
    assert kept[0] | kept[1] == every
    assert not kept[0] & kept[1]


def test_plugin_is_inert_without_a_shard_spec(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv(pytest_shard.SHARD_ENV, raising=False)
    items = _items({"tests/test_a.py": 3})
    pytest_shard.pytest_collection_modifyitems(SimpleNamespace(hook=_Hook()), items)  # type: ignore[arg-type]
    assert len(items) == 3


# ---------------------------------------------------------- run_bundle glue --


@pytest.fixture
def recorded(monkeypatch: pytest.MonkeyPatch) -> dict[str, object]:
    calls: dict[str, object] = {"harnesses": 0, "pytest": []}

    def fake_harnesses(bundle: Path, manifest: dict, env: dict[str, str]) -> int:
        calls["harnesses"] = int(calls["harnesses"]) + 1  # type: ignore[call-overload]
        return 0

    def fake_pytest(marker: str, env: dict[str, str], extra: list[str], *, suite: str) -> int:
        calls["pytest"].append((marker, dict(env), list(extra), suite))  # type: ignore[attr-defined]
        return 0

    monkeypatch.setattr(run_bundle, "run_harnesses", fake_harnesses)
    monkeypatch.setattr(run_bundle, "run_pytest", fake_pytest)
    return calls


def test_rust_shard_runs_harnesses_and_no_pytest(recorded: dict[str, object]) -> None:
    assert run_unit_suite(Path("b"), {}, {}, parse_shard("rust"), []) == 0
    assert recorded["harnesses"] == 1
    assert recorded["pytest"] == []


def test_python_shard_runs_pytest_with_its_slice_and_no_harnesses(
    recorded: dict[str, object],
) -> None:
    assert run_unit_suite(Path("b"), {}, {"X": "1"}, parse_shard("py2of2"), ["-q"]) == 0
    assert recorded["harnesses"] == 0
    ((marker, env, extra, suite),) = recorded["pytest"]  # type: ignore[misc]
    assert (marker, suite) == ("not integration", "unit")
    assert env["CLUD_PYTEST_SHARD"] == "2/2"
    assert env["X"] == "1"
    assert extra == ["-p", "ci.pytest_shard", "-q"]


def test_all_runs_everything_unsharded(recorded: dict[str, object]) -> None:
    assert run_unit_suite(Path("b"), {}, {}, parse_shard("all"), []) == 0
    assert recorded["harnesses"] == 1
    ((_, env, extra, _),) = recorded["pytest"]  # type: ignore[misc]
    assert "CLUD_PYTEST_SHARD" not in env
    assert "ci.pytest_shard" not in extra


def test_a_failing_rust_shard_skips_pytest(monkeypatch: pytest.MonkeyPatch) -> None:
    def no_pytest(*_args: object, **_kwargs: object) -> int:
        pytest.fail("pytest must not run after a Rust failure")

    monkeypatch.setattr(run_bundle, "run_harnesses", lambda *_: 1)
    monkeypatch.setattr(run_bundle, "run_pytest", no_pytest)
    assert run_unit_suite(Path("b"), {}, {}, parse_shard("all"), []) == 1


# ------------------------------------------------- workflow + release gate --


def test_the_unit_job_is_a_matrix_over_exactly_the_declared_shards() -> None:
    text = CI_YML.read_text(encoding="utf-8")
    block = text.split("\n  test-linux-x64-unit:\n", 1)[1].split(
        "\n  test-linux-x64-integration:\n", 1
    )[0]
    listed = re.search(r"^        shard: \[([^\]]+)\]$", block, re.MULTILINE)
    assert listed, "test-linux-x64-unit has no shard matrix"
    assert tuple(part.strip() for part in listed.group(1).split(",")) == LINUX_X64_UNIT_SHARDS
    assert "shard: ${{ matrix.shard }}" in block
    assert "suite: unit" in block
    assert "fail-fast: false" in block


def test_the_reusable_workflow_forwards_the_shard() -> None:
    text = RUN_TESTS.read_text(encoding="utf-8")
    assert "--shard ${{ inputs.shard }}" in text
    # Matrix shards upload concurrently; a shared artifact name would collide.
    assert "inputs.shard != 'all'" in text


def test_release_gate_requires_one_job_per_shard_and_no_bare_linux_unit_job() -> None:
    triple = TARGETS[0].triple
    for shard in LINUX_X64_UNIT_SHARDS:
        assert f"Test linux-x64 (unit) ({shard}) / {triple} unit" in release_gate.REQUIRED_JOBS
    assert f"Test linux-x64 (unit) / {triple} unit" not in release_gate.REQUIRED_JOBS
    # Every other lane keeps its single unit cell.
    assert "Test windows-x64 (unit) / x86_64-pc-windows-msvc unit" in release_gate.REQUIRED_JOBS
