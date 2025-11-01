"""Pytest configuration and fixtures."""

import pytest  # pyright: ignore[reportMissingImports]


@pytest.fixture(scope="session")  # pyright: ignore[reportUnknownMemberType, reportUntypedFunctionDecorator]
def anyio_backend() -> str:
    """Use asyncio backend only (trio is not installed)."""
    return "asyncio"
