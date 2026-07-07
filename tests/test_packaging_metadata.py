"""Tests for Python packaging metadata needed by local builds."""

from __future__ import annotations

from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parent.parent


def _pyproject() -> dict:
    with (ROOT / "pyproject.toml").open("rb") as handle:
        return tomllib.load(handle)


def test_pip_build_uses_clud_soldr_backend_wrapper() -> None:
    build_system = _pyproject()["build-system"]
    requirements = [requirement.lower() for requirement in build_system["requires"]]

    assert build_system["build-backend"] == "build_backend"
    assert build_system["backend-path"] == ["."]
    assert any(requirement.startswith("soldr") for requirement in requirements)
    assert any("platform_system" in requirement for requirement in requirements)
    assert not any(requirement.startswith("maturin") for requirement in requirements)
    assert not any(requirement.startswith("cmake") for requirement in requirements)
