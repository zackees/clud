from __future__ import annotations

from pathlib import Path

import pytest

from tests.harness.harness import ENABLED, Harness


@pytest.hookimpl(tryfirst=True)
def pytest_collection_modifyitems(items: list[pytest.Item]) -> None:
    """Directory conftest marks must be attached to actual collected items."""
    root = Path(__file__).parent
    for item in items:
        if item.path.is_relative_to(root):
            item.add_marker(pytest.mark.real_claude)


@pytest.fixture
def harness(tmp_path: Path) -> Harness:
    if not ENABLED:
        pytest.skip("set CLUD_REAL_CLAUDE_TESTS=1 to run the real Claude Code harness tests")
    h = Harness(tmp_path)
    h.make_repo()
    h.install_assets()
    return h
