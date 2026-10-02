"""Which processes `ci/run_bundle.py` starts for one Rust harness (#1726).

Most harnesses run whole, in one process. The `integration` harness holds
every category of clud's integration tests (`api`, `cli`, `diagnostics`,
`pty`, `reaper`, `signals`) since #1726 folded six binaries into one. CI keeps
the isolation the separate binaries had: each category runs in a process of
its own, so process-wide state such as the reaper's host-wide sweeps never
meets another category's tests, and each `pty` test runs in its own
pseudo-terminal (#691: ConPTY needs a console; #1310: a hang names one test).

Tests are selected by exact name, never by a `<category>::` substring, which
would also match a module or test elsewhere whose name ends in that word.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

#: The one integration harness; Cargo names its binary `integration-<hash>[.exe]`.
SPLIT_HARNESS = "integration"
#: Its category whose tests drive a real PTY (`tests/integration/pty/`).
TERMINAL_CATEGORY = "pty"
#: Budget for one process's exact-name filters, well under Windows' 32767-char
#: command line.
MAX_FILTER_CHARS = 8000


@dataclass(frozen=True)
class HarnessRun:
    """One process: a category's tests selected by exact name."""

    category: str
    tests: tuple[str, ...]
    terminal: bool

    def argv(self, base: list[str]) -> list[str]:
        return [*base, "--exact", *self.tests]


def harness_name(harness: Path) -> str:
    """`integration-<hash>[.exe]` -> `integration`."""
    stem = harness.name.removesuffix(".exe")
    name, sep, _hash = stem.rpartition("-")
    return name if sep else stem


def is_split(harness: Path) -> bool:
    """True for the integration harness, which runs one category per process."""
    stem = harness.name.removesuffix(".exe")
    return "-" in stem and harness_name(harness) == SPLIT_HARNESS


def _batches(names: list[str]) -> list[tuple[str, ...]]:
    batches: list[tuple[str, ...]] = []
    current: list[str] = []
    size = 0
    for name in names:
        cost = len(name) + 1
        if current and size + cost > MAX_FILTER_CHARS:
            batches.append(tuple(current))
            current, size = [], 0
        current.append(name)
        size += cost
    if current:
        batches.append(tuple(current))
    return batches


def split_runs(names: list[str]) -> list[HarnessRun]:
    """The processes for the integration harness, given its `--list` names."""
    categories: dict[str, list[str]] = {}
    for name in names:
        categories.setdefault(name.split("::", 1)[0], []).append(name)
    runs: list[HarnessRun] = []
    for category in sorted(categories):
        members = categories[category]
        if category == TERMINAL_CATEGORY:
            runs.extend(HarnessRun(category, (name,), True) for name in members)
        else:
            runs.extend(HarnessRun(category, batch, False) for batch in _batches(members))
    return runs
