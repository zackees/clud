"""Tests for CI environment command selection."""

from __future__ import annotations

from ci import env as ci_env


def test_cargo_argv_prefers_explicit_cargo(monkeypatch) -> None:
    monkeypatch.setattr(ci_env, "soldr_path", lambda env=None: "soldr")

    assert ci_env.cargo_argv(["check"], env={"CARGO": r"C:\rust\bin\cargo.exe"}) == [
        r"C:\rust\bin\cargo.exe",
        "check",
    ]


def test_cargo_argv_uses_soldr_when_no_explicit_cargo(monkeypatch) -> None:
    monkeypatch.setattr(ci_env, "soldr_path", lambda env=None: "soldr")

    assert ci_env.cargo_argv(["check"], env={}) == ["soldr", "cargo", "check"]


def test_cargo_argv_falls_back_to_bare_cargo(monkeypatch) -> None:
    monkeypatch.setattr(ci_env, "soldr_path", lambda env=None: None)

    assert ci_env.cargo_argv(["check"], env={}) == ["cargo", "check"]
