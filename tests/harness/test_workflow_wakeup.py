"""Prove the native scheduler wakes an idle session without an external loop."""
from __future__ import annotations

import json

import pytest

from tests.harness.harness import Harness


@pytest.mark.timeout(240)
def test_native_loop_scheduler_wakes_and_continues(harness: Harness) -> None:
    script = {"roles": [{"name": "main", "steps": [
        {"tool_use": {"name": "CronCreate", "input": {
            # The interactive harness stops active tasks on exit after the first wake.
            "cron": "* * * * *", "prompt": "WAKE_PUBLICATION_PROBE", "recurring": True,
        }}},
        {"expect": {"is_error": False}, "text": "PUBLICATION_WAKE_ARMED"},
        {"tool_use": {"name": "Bash", "input": {
            "command": "printf continued > native-wakeup.txt",
            "description": "Record continuation after the native scheduler wakeup",
        }}},
        {"expect": {"is_error": False}, "text": "NATIVE_PUBLICATION_WAKE_COMPLETE"},
    ]}]}
    result = harness.run_clud_interactive(
        "/loop 1m Continue the controlled publication probe when scheduled.", script,
        model="nvidia/nemotron-3-ultra-550b-a55b:free",
        finished="NATIVE_PUBLICATION_WAKE_COMPLETE", timeout=180,
    )
    assert result.returncode == 0, result.stdout[-4000:]
    assert not [row for row in result.requests if row.get("note")]
    assert (harness.repo / "native-wakeup.txt").read_text() == "continued"
    assert any(row.get("event") == "PostToolUse" and row.get("tool_name") == "CronCreate"
               for row in result.hooks)
    resumed = [row for row in result.requests if row.get("step", {}).get("tool_use", {}).get(
        "name") == "Bash"]
    assert len(resumed) == 1, resumed
    latest = resumed[0]["messages"][-1]
    assert latest["role"] == "user", latest
    assert "WAKE_PUBLICATION_PROBE" in json.dumps(latest), latest
