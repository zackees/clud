"""The cross-platform next-PATH test executable cannot delete operands."""

from __future__ import annotations

import json
import os
from pathlib import Path

from tests import process


def test_mock_agent_recording_handoff_logs_only_argv(tmp_path: Path) -> None:
    binary = Path(os.environ["CLUD_TEST_MOCK_AGENT_BINARY"])
    log = tmp_path / "args.json"
    env = os.environ.copy()
    env.update(HOME=str(tmp_path), USERPROFILE=str(tmp_path), MOCK_RM_STUB_LOG=str(log))
    result = process.run(
        [str(binary), "-rf", str(tmp_path / "target")],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 0, result
    assert json.loads(log.read_text(encoding="utf-8")) == ["-rf", str(tmp_path / "target")]
