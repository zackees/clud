"""Split the unit pytest suite across independent runners, by file.

The Linux x64 unit lane used to be one job: ~55 s of Rust harnesses, then
~135 s of serial pytest. Both halves are CPU-bound (every test launches real
`clud` processes) and a hosted "4 vCPU" runner is two physical cores, so more
workers on one machine only contend (pytest-xdist measured *slower*: 172 s).
Separate machines do not contend, so the lane is now three parallel jobs:
the Rust harnesses, and two halves of the pytest suite.

This module is the pytest half of that split. `ci/run_bundle.py` passes
`-p ci.pytest_shard --clud-shard <k>/<n>`; every shard collects the
full suite and keeps the files `assign_files` gives it. The assignment is a
pure function of (file list, weights, shard count), so the shards are disjoint
and cover the suite exactly when they all see the same collection, which the
shared `-m "not integration"` deselection guarantees.

Weights are seconds per file, from `ci/unit_shard_weights.json`. Stale or
missing entries only cost balance, never coverage: an unknown file is weighted
by its test count.

The spec travels as a command-line option, not an environment variable:
`tests/conftest.py` scrubs every `CLUD_*` variable that is not test-harness
configuration, which silently turned the first version of this split (an env
var) into "every shard runs the whole suite". `applied_marker` is the line
`run_bundle` looks for afterwards so that failure mode cannot recur unnoticed.
"""

from __future__ import annotations

import json
import re
from collections.abc import Mapping
from pathlib import Path

import pytest

SHARD_OPTION = "--clud-shard"
WEIGHTS_PATH = Path(__file__).with_name("unit_shard_weights.json")
#: Weight of one test in a file with no recorded timing (~ suite average).
DEFAULT_SECONDS_PER_TEST = 0.09

_SPEC = re.compile(r"^(\d+)/(\d+)$")


def parse_spec(spec: str) -> tuple[int, int]:
    """`"2/3"` -> `(1, 3)`: a zero-based shard index and the shard count."""
    match = _SPEC.fullmatch(spec.strip())
    if match is None:
        raise ValueError(f"{SHARD_OPTION} must look like '<k>/<n>', got {spec!r}")
    number, count = int(match.group(1)), int(match.group(2))
    if count < 1 or not 1 <= number <= count:
        raise ValueError(f"{SHARD_OPTION} {spec!r}: need 1 <= k <= n")
    return number - 1, count


def load_weights(path: Path = WEIGHTS_PATH) -> dict[str, float]:
    """Recorded seconds per test file; a missing or unreadable file means none."""
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    # A parseable file of the wrong shape only costs balance, never the run.
    if not isinstance(raw, dict):
        return {}
    return {
        str(name): float(seconds)
        for name, seconds in raw.items()
        if isinstance(seconds, (int, float)) and not isinstance(seconds, bool)
    }


def assign_files(
    counts: Mapping[str, int], weights: Mapping[str, float], shard_count: int
) -> dict[str, int]:
    """Longest-processing-time assignment of files to `shard_count` shards.

    Heaviest file first, each onto the currently lightest shard; ties break on
    file name so every shard computes the same answer.
    """
    if shard_count < 1:
        raise ValueError("shard_count must be at least 1")

    def weight(name: str) -> float:
        recorded = weights.get(name)
        return recorded if recorded is not None else counts[name] * DEFAULT_SECONDS_PER_TEST

    loads = [0.0] * shard_count
    assignment: dict[str, int] = {}
    for name in sorted(counts, key=lambda file: (-weight(file), file)):
        lightest = loads.index(min(loads))
        assignment[name] = lightest
        loads[lightest] += weight(name)
    return assignment


def applied_marker(spec: str) -> str:
    """The line the plugin prints once it has filtered a collection to `spec`."""
    index, count = parse_spec(spec)
    return f"pytest shard {index + 1}/{count}:"


def pytest_addoption(parser: pytest.Parser) -> None:
    parser.addoption(
        SHARD_OPTION,
        default=None,
        metavar="K/N",
        help="run only shard K of N of the collected test files (ci/pytest_shard.py)",
    )


@pytest.hookimpl(trylast=True)
def pytest_collection_modifyitems(config: pytest.Config, items: list[pytest.Item]) -> None:
    spec = config.getoption(SHARD_OPTION)
    if not spec:
        return
    index, count = parse_spec(spec)
    counts: dict[str, int] = {}
    for item in items:
        name = item.nodeid.split("::", 1)[0]
        counts[name] = counts.get(name, 0) + 1
    assignment = assign_files(counts, load_weights(), count)
    kept = [item for item in items if assignment[item.nodeid.split("::", 1)[0]] == index]
    dropped = [item for item in items if assignment[item.nodeid.split("::", 1)[0]] != index]
    if dropped:
        config.hook.pytest_deselected(items=dropped)
    items[:] = kept
    total = len(kept) + len(dropped)
    print(f"{applied_marker(spec)} {len(kept)} of {total} tests", flush=True)
