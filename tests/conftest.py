"""Shared pytest configuration for clud tests."""

from __future__ import annotations

import os

import pytest


def pytest_collection_modifyitems(config: pytest.Config, items: list[pytest.Item]) -> None:
    """Skip integration tests unless CLUD_INTEGRATION_TESTS=1."""
    if os.environ.get("CLUD_INTEGRATION_TESTS", "").lower() in {"1", "true", "yes"}:
        return
    skip_integration = pytest.mark.skip(reason="set CLUD_INTEGRATION_TESTS=1 to run")
    for item in items:
        if "integration" in item.keywords:
            item.add_marker(skip_integration)
