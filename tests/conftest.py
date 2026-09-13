"""Shared pytest configuration for clud tests."""

from __future__ import annotations

import importlib.util
import os
import sys
from pathlib import Path

import pytest


def pytest_collection_modifyitems(config: pytest.Config, items: list[pytest.Item]) -> None:
    """Skip integration tests unless CLUD_INTEGRATION_TESTS=1."""
    if os.environ.get("CLUD_INTEGRATION_TESTS", "").lower() in {"1", "true", "yes"}:
        return
    skip_integration = pytest.mark.skip(reason="set CLUD_INTEGRATION_TESTS=1 to run")
    for item in items:
        if "integration" in item.keywords:
            item.add_marker(skip_integration)


@pytest.fixture
def bridge():
    """Load `src/clud/mcp_server.py` as an importable module.

    Shared here (not in the test module) so both the original bridge tests and
    the session-surface tests drive the same live module.
    """
    name = "clud_test_mcp_server"
    script = Path(__file__).resolve().parents[1] / "src" / "clud" / "mcp_server.py"
    spec = importlib.util.spec_from_file_location(name, script)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    try:
        yield module
    finally:
        sys.modules.pop(name, None)
