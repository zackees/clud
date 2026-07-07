"""Tests for Python packaging metadata needed by local builds."""

from __future__ import annotations

import re
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
    maturin_requirements = [
        requirement for requirement in requirements if requirement.startswith("maturin")
    ]
    assert all(
        "darwin" in requirement and "x86_64" in requirement
        for requirement in maturin_requirements
    )
    assert not any(requirement.startswith("cmake") for requirement in requirements)


def test_ci_setup_soldr_pins_backend_compatible_soldr() -> None:
    workflow_paths = sorted((ROOT / ".github" / "workflows").glob("_*.yml"))
    version_lines = []
    for path in workflow_paths:
        text = path.read_text(encoding="utf-8")
        if "zackees/setup-soldr" not in text:
            continue
        version_lines.extend(
            line for line in re.findall(r"version:\s*(.+)", text) if "0.7." in line
        )

    assert version_lines
    assert all("0.7.104" in line for line in version_lines)
    assert all("0.7.45" not in line or "inputs.runs-on" in line for line in version_lines)
