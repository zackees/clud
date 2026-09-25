from __future__ import annotations

from pathlib import Path

import pytest

from tests.harness.harness import ENABLED, Harness

pytestmark = pytest.mark.real_claude


@pytest.fixture
def harness(tmp_path: Path) -> Harness:
    if not ENABLED:
        pytest.skip("set CLUD_REAL_CLAUDE_TESTS=1 to run the real Claude Code harness tests")
    h = Harness(tmp_path)
    h.make_repo()
    h.install_assets()
    return h
