"""Command-routing regression coverage for the soldr Docker stack."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[1]
    / "crates"
    / "clud-bin"
    / "assets"
    / "tools"
    / "docker"
    / "docker_build_soldr.py"
)


def _load_module():
    name = "clud_test_docker_build_soldr_run"
    spec = importlib.util.spec_from_file_location(name, SCRIPT)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def test_raw_cargo_commands_route_through_soldr() -> None:
    module = _load_module()

    assert module.managed_run_command(["cargo", "test", "-p", "clud"]) == [
        "soldr",
        "cargo",
        "test",
        "-p",
        "clud",
    ]
    assert module.managed_run_command(["/usr/local/bin/cargo", "check"]) == [
        "soldr",
        "cargo",
        "check",
    ]


def test_non_cargo_commands_are_unchanged() -> None:
    module = _load_module()

    assert module.managed_run_command(["cmake", "--version"]) == [
        "cmake",
        "--version",
    ]
    assert module.managed_run_command(["soldr", "cargo", "test"]) == [
        "soldr",
        "cargo",
        "test",
    ]
