"""Gating for running the unit suite's pytest half under pytest-xdist.

The Python half of the Linux unit lane was ~135 s of serial pytest on a
4-vCPU runner, the longest step of the PR critical path. `ci/run_bundle.py`
now runs it with `-n auto --dist loadfile` on Linux. Everything that must stay
serial has to stay serial: the Windows/macOS lanes (shared console/PTY
state), the integration and harness suites (real daemons and PTYs), and any
venv that lacks the plugin.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from ci import run_bundle
from ci.run_bundle import run_pytest, xdist_args


@pytest.fixture
def linux_with_xdist(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(run_bundle.sys, "platform", "linux")
    monkeypatch.setattr(run_bundle.importlib.util, "find_spec", lambda name: object())
    monkeypatch.delenv("CLUD_PYTEST_SERIAL", raising=False)
    monkeypatch.delenv("CLUD_PYTEST_WORKERS", raising=False)


def test_linux_unit_suite_runs_under_xdist_by_file(linux_with_xdist: None) -> None:
    assert xdist_args("unit") == ["-n", "auto", "--dist", "loadfile"]


@pytest.mark.parametrize("suite", ["integration", "harness"])
def test_suites_that_drive_real_daemons_stay_serial(linux_with_xdist: None, suite: str) -> None:
    assert xdist_args(suite) == []


@pytest.mark.parametrize("platform", ["win32", "darwin"])
def test_windows_and_macos_stay_serial(
    linux_with_xdist: None, monkeypatch: pytest.MonkeyPatch, platform: str
) -> None:
    monkeypatch.setattr(run_bundle.sys, "platform", platform)
    assert xdist_args("unit") == []


def test_serial_escape_hatch_disables_xdist(
    linux_with_xdist: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("CLUD_PYTEST_SERIAL", "1")
    assert xdist_args("unit") == []


def test_worker_count_can_be_overridden(
    linux_with_xdist: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("CLUD_PYTEST_WORKERS", "2")
    assert xdist_args("unit") == ["-n", "2", "--dist", "loadfile"]


def test_a_venv_without_xdist_runs_serially_instead_of_failing(
    linux_with_xdist: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(run_bundle.importlib.util, "find_spec", lambda name: None)
    assert xdist_args("unit") == []


def _captured_argv(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, marker: str, suite: str
) -> list[str]:
    seen: list[list[str]] = []

    def fake_run_streamed(argv: list[str], env: dict[str, str], log_path: Path) -> int:
        seen.append(argv)
        return 0

    monkeypatch.setattr(run_bundle, "run_streamed", fake_run_streamed)
    monkeypatch.setattr(run_bundle, "LOG_DIR", tmp_path)
    assert run_pytest(marker, {}, ["-q"], suite=suite) == 0
    return seen[0]


def test_run_pytest_passes_xdist_only_to_the_unit_suite(
    linux_with_xdist: None, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    unit = _captured_argv(monkeypatch, tmp_path, "not integration", "unit")
    assert unit[unit.index("-n") + 1] == "auto"
    assert "--dist" in unit and "loadfile" in unit
    assert "-p" in unit and "ci.pytest_progress" in unit  # the progress journal still loads
    assert unit[-1] == "-q"  # caller-supplied args stay last

    integration = _captured_argv(monkeypatch, tmp_path, "integration", "integration")
    assert "-n" not in integration
