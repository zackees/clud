"""Tests for CI test-suite selection."""

from __future__ import annotations

from ci import test as ci_test


def test_select_suites_defaults_to_unit_only() -> None:
    assert ci_test._select_suites([]) == (True, False, [])


def test_select_suites_integration_is_integration_only() -> None:
    assert ci_test._select_suites(["--integration", "-k", "daemon"]) == (
        False,
        True,
        ["-k", "daemon"],
    )


def test_select_suites_full_runs_unit_and_integration() -> None:
    assert ci_test._select_suites(["--full", "-x"]) == (True, True, ["-x"])


def test_prepare_pytest_binaries_reuses_installed_clud(monkeypatch, tmp_path) -> None:
    target_dir = tmp_path / "target" / "debug"
    target_dir.mkdir(parents=True)
    mock_agent = target_dir / ci_test._binary_name("mock-agent")
    mock_agent.write_text("", encoding="utf-8")
    installed_clud = tmp_path / ci_test._binary_name("clud")
    installed_clud.write_text("", encoding="utf-8")
    captured: list[list[str]] = []

    monkeypatch.setattr(ci_test, "_installed_clud_script", lambda: installed_clud)
    monkeypatch.setattr(ci_test, "ROOT", tmp_path)

    def fake_run(cmd: list[str], *, env=None) -> int:
        captured.append(cmd)
        return 0

    monkeypatch.setattr(ci_test, "run", fake_run)

    env = ci_test._prepare_pytest_binaries({}, prefer_installed_clud=True)

    assert env is not None
    assert env["CLUD_TEST_BINARY"] == str(installed_clud)
    assert env["CLUD_TEST_MOCK_AGENT_BINARY"] == str(mock_agent)
    assert captured == [["cargo", "build", "-p", "mock-agent"]]


def test_prepare_pytest_binaries_builds_clud_without_installed_script(
    monkeypatch,
    tmp_path,
) -> None:
    target_dir = tmp_path / "target" / "debug"
    target_dir.mkdir(parents=True)
    clud = target_dir / ci_test._binary_name("clud")
    mock_agent = target_dir / ci_test._binary_name("mock-agent")
    clud.write_text("", encoding="utf-8")
    mock_agent.write_text("", encoding="utf-8")
    captured: list[list[str]] = []

    monkeypatch.setattr(ci_test, "_installed_clud_script", lambda: None)
    monkeypatch.setattr(ci_test, "ROOT", tmp_path)

    def fake_run(cmd: list[str], *, env=None) -> int:
        captured.append(cmd)
        return 0

    monkeypatch.setattr(ci_test, "run", fake_run)

    env = ci_test._prepare_pytest_binaries({}, prefer_installed_clud=True)

    assert env is not None
    assert env["CLUD_TEST_BINARY"] == str(clud)
    assert env["CLUD_TEST_MOCK_AGENT_BINARY"] == str(mock_agent)
    assert captured == [["cargo", "build", "-p", "clud", "-p", "mock-agent"]]
