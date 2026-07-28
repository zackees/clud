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
    assert requirements == ["soldr>=0.8.27"]


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
    """Windows must never run the soldr dependency cook.

    Asserted as an invariant rather than as one exact literal. Two
    settings satisfy it: the usual `runner.os == 'Windows'` conditional,
    and a blanket `none` that disables the cook everywhere.

    The conditional form is in force. A blanket `none` was in force for a
    while as a workaround for zackees/soldr#1880 (hydrate restored
    `build-script-build` binaries without the executable bit, so cargo died
    with "Permission denied (os error 13)" on a rotating, arbitrary crate).
    That was fixed upstream by soldr#1889 and soldr#1914, shipped in
    v0.8.25/v0.8.26 — both below the v0.8.27 this repo pins — so the cook is
    enabled again everywhere except Windows.

    Both forms stay acceptable here because this test guards the Windows
    invariant, not which of the two ways it is currently achieved.
    """
    conditional = "prebuild-deps: ${{ runner.os == 'Windows' && 'none' || 'soldr-cook' }}"
    blanket = "prebuild-deps: none"

    setup_workflows = [
        path
        for path in (ROOT / ".github" / "workflows").glob("_*.yml")
        if "zackees/setup-soldr" in path.read_text(encoding="utf-8")
    ]

    assert setup_workflows
    for path in setup_workflows:
        text = path.read_text(encoding="utf-8")
        assert conditional in text or blanket in text, (
            f"{path.name}: prebuild-deps must either exempt Windows via the "
            f"runner.os conditional or disable the cook entirely with 'none'"
        )
        # Either way, no workflow may hand Windows an unconditional cook.
        assert "prebuild-deps: soldr-cook" not in text, (
            f"{path.name}: unconditional 'soldr-cook' would run the dependency "
            f"cook on Windows"
        )
