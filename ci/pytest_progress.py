"""Flush each pytest test boundary to an artifact independent of stdout."""

from __future__ import annotations

import json
import os
import time
from pathlib import Path


def _record(event: str, nodeid: str) -> None:
    destination = os.environ.get("CLUD_PYTEST_PROGRESS_LOG")
    if not destination:
        return
    path = Path(destination)
    path.parent.mkdir(parents=True, exist_ok=True)
    entry = {"event": event, "nodeid": nodeid, "ts_ns": time.time_ns()}
    with path.open("a", encoding="utf-8") as output:
        output.write(json.dumps(entry, separators=(",", ":")) + "\n")


def pytest_runtest_logstart(nodeid: str, location: tuple[str, int | None, str]) -> None:
    _record("start", nodeid)


def pytest_runtest_logfinish(nodeid: str, location: tuple[str, int | None, str]) -> None:
    _record("finish", nodeid)
