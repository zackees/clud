"""The integration mock answers Codex's startup model discovery."""

from __future__ import annotations

import json
import os
from pathlib import Path

from tests import process


def test_mock_codex_app_server_answers_model_list(tmp_path: Path) -> None:
    binary = Path(os.environ["CLUD_TEST_MOCK_AGENT_BINARY"])
    requests = [
        {"method": "initialize", "id": 0, "params": {"clientInfo": {"name": "clud"}}},
        {"method": "initialized", "params": {}},
        {"method": "model/list", "id": 1, "params": {"limit": 100}},
    ]
    result = process.run(
        [str(binary), "app-server"],
        input="".join(json.dumps(request) + "\n" for request in requests),
        cwd=tmp_path,
        capture_output=True,
        text=True,
        timeout=5,
    )
    assert result.returncode == 0, result.stderr
    frames = [json.loads(line) for line in result.stdout.splitlines()]
    assert frames == [
        {"id": 0, "result": {"capabilities": {}}},
        {"id": 1, "result": {"data": [], "nextCursor": None}},
    ]
