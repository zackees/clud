"""Tests for Python packaging metadata needed by local builds."""

from __future__ import annotations

import re
from pathlib import Path

import tomllib
from packaging.version import Version

ROOT = Path(__file__).resolve().parent.parent
MIN_SOLDR_VERSION = Version("0.7.98")


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


def test_ci_setup_soldr_pins_backend_compatible_soldr() -> None:
    workflow_paths = sorted((ROOT / ".github" / "workflows").glob("_*.yml"))
    version_pins = []
    for path in workflow_paths:
        text = path.read_text(encoding="utf-8")
        if "zackees/setup-soldr" not in text:
            continue
        version_pins.extend(re.findall(r'version:\s*"([0-9.]+)"', text))

    assert version_pins
    assert all(Version(pin) >= MIN_SOLDR_VERSION for pin in version_pins)
