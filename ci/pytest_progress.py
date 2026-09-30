"""Flush each pytest test boundary to an artifact independent of stdout."""

from __future__ import annotations

import json
import os
import time
from pathlib import Path

# Read once, at plugin import. `-p ci.pytest_progress` is imported while pytest
# parses its arguments, before `tests/conftest.py` runs, and that conftest
# scrubs every unlisted `CLUD_*` variable from `os.environ` so clud children
# spawned by tests start clean (#1423). Reading the variable per test would
# find it already gone and never write the journal (#1625). The scrub stays:
# the path is pytest-process state, not something a test's children need.
_DESTINATION = os.environ.get("CLUD_PYTEST_PROGRESS_LOG")


def _record(event: str, nodeid: str) -> None:
    if not _DESTINATION:
        return
    path = Path(_DESTINATION)
    path.parent.mkdir(parents=True, exist_ok=True)
    entry = {"event": event, "nodeid": nodeid, "ts_ns": time.time_ns()}
    with path.open("a", encoding="utf-8") as output:
        output.write(json.dumps(entry, separators=(",", ":")) + "\n")


def pytest_runtest_logstart(nodeid: str, location: tuple[str, int | None, str]) -> None:
    _record("start", nodeid)


def pytest_runtest_logfinish(nodeid: str, location: tuple[str, int | None, str]) -> None:
    _record("finish", nodeid)
