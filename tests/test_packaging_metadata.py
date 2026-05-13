"""Tests for Python packaging metadata needed by local builds."""

from __future__ import annotations

import ast
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def _build_system_requires() -> list[str]:
    text = (ROOT / "pyproject.toml").read_text(encoding="utf-8")
    section_match = re.search(r"(?ms)^\[build-system\]\s*(.*?)(?=^\[|\Z)", text)
    if section_match is None:
        raise AssertionError("missing [build-system] section")

    requires_match = re.search(r"(?ms)^requires\s*=\s*(\[.*?\])", section_match.group(1))
    if requires_match is None:
        raise AssertionError("missing build-system.requires")

    return ast.literal_eval(requires_match.group(1))


def test_pip_build_requires_native_build_tools() -> None:
    requirements = [requirement.lower() for requirement in _build_system_requires()]

    assert any(requirement.startswith("maturin") for requirement in requirements)
    assert any(requirement.startswith("cmake") for requirement in requirements)
