"""Tier 0 / Tier 1 test split (#2356).

This module must stay Tier 0, so it names the binary variables only through
`ci.test`: spelling one out here would classify the module `daemon`.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from ci import test as ci_test
from tests import conftest


def test_host_tier_points_every_binary_variable_at_a_missing_file() -> None:
    tier0 = ci_test.host_tier_env({"PATH": "/bin"})
    assert tier0["PATH"] == "/bin"
    for name in ci_test._BINARY_ENV_VARS:
        path = Path(tier0[name])
        assert path.name == ci_test.TIER0_ABSENT_BINARY
        assert not path.exists()


def test_host_tier_does_not_mutate_the_caller_env() -> None:
    env = {"PATH": "/bin"}
    ci_test.host_tier_env(env)
    assert env == {"PATH": "/bin"}


def test_host_flag_dispatches_to_the_host_tier(monkeypatch: pytest.MonkeyPatch) -> None:
    seen: list[list[str]] = []
    monkeypatch.setattr(ci_test, "run_host_tier", lambda env, args: seen.append(args) or 0)
    monkeypatch.setattr("ci.env.activate", lambda: None)
    monkeypatch.setattr("ci.env.suite_env", dict)
    assert ci_test.main(["--host", "-k", "tiers"]) == 0
    assert seen == [["-k", "tiers"]]


class _Item:
    def __init__(self, path: Path) -> None:
        self.path = path
        self.markers: list[str] = []

    def add_marker(self, marker: pytest.MarkDecorator) -> None:
        self.markers.append(marker.name)


def test_modules_that_name_a_built_binary_are_marked_daemon(tmp_path: Path) -> None:
    binary = tmp_path / "test_runs_binary.py"
    binary.write_text(f"BIN = {ci_test._BINARY_ENV_VARS[0]!r}\n", encoding="utf-8")
    pure = tmp_path / "test_pure.py"
    pure.write_text("def test_x():\n    assert True\n", encoding="utf-8")
    items = [_Item(binary), _Item(pure)]
    conftest.mark_binary_tests_daemon(items)  # type: ignore[arg-type]
    assert items[0].markers == ["daemon"]
    assert items[1].markers == []


def test_this_module_stays_tier0() -> None:
    assert not conftest._runs_built_binary(Path(__file__))


def test_a_module_importing_a_binary_module_is_tier1(tmp_path: Path) -> None:
    helper = tmp_path / "zz_tier_helper.py"
    helper.write_text(f"BIN = {ci_test._BINARY_ENV_VARS[0]!r}\n", encoding="utf-8")
    user = tmp_path / "test_zz_tier_user.py"
    user.write_text("from zz_tier_helper import BIN\n", encoding="utf-8")
    assert conftest._runs_built_binary(user)
