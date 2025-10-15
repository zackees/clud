"""Pytest configuration and fixtures."""

import pytest


@pytest.fixture(scope="session")
def anyio_backend() -> str:
    """Use asyncio backend only (trio is not installed)."""
    return "asyncio"
