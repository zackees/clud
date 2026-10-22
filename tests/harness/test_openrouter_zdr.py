"""A saved ZDR preference reaches actual direct and unified HTTP requests."""

from __future__ import annotations

import pytest

from tests import process
from tests.harness.harness import Harness


@pytest.mark.parametrize(
    ("provider", "model"),
    [("--openrouter", None), ("--unified", "openrouter-claude-sonnet")],
)
def test_saved_zdr_reaches_native_requests(
    harness: Harness, provider: str, model: str | None
) -> None:
    saved = process.run(
        [str(harness.clud), "--set-openrouter-zdr"],
        env=harness.env({"CLUD_NO_UNLOCK": "1", "CLUD_SERVER_SETTINGS": "0"}),
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert saved.returncode == 0, saved.stdout + saved.stderr
    result = harness.run_clud(
        "Reply with the completion marker",
        {"default_text": "SAVED_ZDR_REQUEST_COMPLETE"},
        provider=provider,
        model=model,
    )
    assert result.returncode == 0, result.stdout[-3000:] + result.stderr[-3000:]
    assert "SAVED_ZDR_REQUEST_COMPLETE" in result.stdout
    assert result.requests, "the real Claude process must reach the scripted backend"
    for request in result.requests:
        assert request["provider"]["zdr"] is True, request
        assert "zdr_source" not in request["provider"]
