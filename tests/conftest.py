"""Pytest configuration and fixtures."""

from pathlib import Path

import pytest  # pyright: ignore[reportMissingImports]


@pytest.fixture(scope="session")  # pyright: ignore[reportUnknownMemberType, reportUntypedFunctionDecorator]
def anyio_backend() -> str:
    """Use asyncio backend only (trio is not installed)."""
    return "asyncio"


@pytest.fixture(autouse=True)  # pyright: ignore[reportUnknownMemberType, reportUntypedFunctionDecorator]
def isolate_home_directory(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    """Isolate HOME/USERPROFILE so tests never read or write real user settings."""
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("USERPROFILE", str(tmp_path))
    monkeypatch.setenv("HOMEDRIVE", tmp_path.drive or "C:")
    monkeypatch.setenv("HOMEPATH", tmp_path.root)


@pytest.fixture  # pyright: ignore[reportUnknownMemberType, reportUntypedFunctionDecorator]
def snap_compare() -> None:
    """Skip snapshot tests when the external snapshot plugin is not installed."""
    pytest.skip("snap_compare fixture requires the snapshot test plugin, which is not installed in this environment")
