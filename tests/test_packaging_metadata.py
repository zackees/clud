"""Tests for Python packaging metadata needed by local builds."""

from __future__ import annotations

import re
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parent.parent


def _pyproject() -> dict:
    with (ROOT / "pyproject.toml").open("rb") as handle:
        return tomllib.load(handle)


def test_pip_build_uses_soldr_pep517_backend() -> None:
    build_system = _pyproject()["build-system"]
    requirements = [requirement.lower() for requirement in build_system["requires"]]

    assert build_system["build-backend"] == "soldr"
    assert "backend-path" not in build_system
    assert requirements == ["soldr>=0.8.24"]


def test_ci_setup_soldr_pins_backend_compatible_soldr() -> None:
    workflow_paths = sorted((ROOT / ".github" / "workflows").glob("*.yml"))
    action_pins = []
    version_lines = []
    for path in workflow_paths:
        text = path.read_text(encoding="utf-8")
        if "zackees/setup-soldr" not in text:
            continue
        action_pins.extend(re.findall(r"uses:\s*zackees/setup-soldr@(\S+)", text))
        version_lines.extend(
            line
            for line in re.findall(r"version:\s*(.+)", text)
            if "0.8.0" in line or "0.7." in line
        )

    assert action_pins
    assert all(pin == "v0.9.66" for pin in action_pins)
    assert version_lines
    assert all("0.8.0" in line for line in version_lines)
    assert all("0.7.104" not in line for line in version_lines)
    assert all(
        "0.7.45" not in line or ("inputs.runs-on" in line and "intel" in line)
        for line in version_lines
    )


def test_ci_setup_soldr_skips_dependency_cook_on_windows() -> None:
    setup_workflows = [
        path
        for path in (ROOT / ".github" / "workflows").glob("_*.yml")
        if "zackees/setup-soldr" in path.read_text(encoding="utf-8")
    ]

    assert setup_workflows
    for path in setup_workflows:
        text = path.read_text(encoding="utf-8")
        assert (
            "prebuild-deps: ${{ runner.os == 'Windows' && 'none' || 'soldr-cook' }}"
            in text
        )
