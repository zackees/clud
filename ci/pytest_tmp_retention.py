"""Remove a passing run's pytest base temp; keep and name a failing one (#1686).

`tmp_path_retention_policy = "failed"` in pyproject already asks pytest to
drop a passing session's base temp, but pytest removes it with
`rmtree(ignore_errors=True)`. Harness worlds hold git objects, which are
read-only, so on Windows that removal silently leaves the whole tree behind
under `$TMPDIR` (`~/.clud/tmp` inside a clud session). This plugin finishes the
job with a removal that clears the read-only bit, and on a failing run prints
where the kept worlds are.

A `--basetemp` the user passed is theirs: it is never removed.
"""

from __future__ import annotations

import os
import shutil
import stat
from pathlib import Path

import pytest


def _clear_readonly_and_retry(func, path, _exc) -> None:
    try:
        os.chmod(path, stat.S_IWRITE | stat.S_IREAD | stat.S_IEXEC)
        parent = os.path.dirname(path)
        if parent:
            os.chmod(parent, stat.S_IWRITE | stat.S_IREAD | stat.S_IEXEC)
        func(path)
    except OSError:
        pass


def remove_tree(path: Path) -> None:
    """Best-effort recursive delete that also removes read-only entries."""
    # `onerror` is deprecated from 3.12 in favour of `onexc`; both receive
    # (func, path, ...) and the handler ignores the third argument.
    try:
        shutil.rmtree(path, onexc=_clear_readonly_and_retry)  # type: ignore[call-arg]
    except TypeError:
        shutil.rmtree(path, onerror=_clear_readonly_and_retry)


def _managed_basetemp(config: pytest.Config) -> Path | None:
    factory = getattr(config, "_tmp_path_factory", None)
    if factory is None or factory._given_basetemp is not None:
        return None
    if factory._retention_policy != "failed":
        return None
    return factory._basetemp


@pytest.hookimpl(trylast=True)
def pytest_sessionfinish(session: pytest.Session, exitstatus: int) -> None:
    if exitstatus != 0:
        return
    basetemp = _managed_basetemp(session.config)
    if basetemp is not None and basetemp.exists():
        remove_tree(basetemp)


def pytest_terminal_summary(terminalreporter, exitstatus: int, config: pytest.Config) -> None:
    if exitstatus == 0:
        return
    factory = getattr(config, "_tmp_path_factory", None)
    basetemp = getattr(factory, "_basetemp", None)
    if basetemp is not None and Path(basetemp).exists():
        terminalreporter.write_line(f"pytest tmp_path worlds kept for the failed run: {basetemp}")
