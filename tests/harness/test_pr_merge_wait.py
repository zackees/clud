"""Drive the real merge waiter through Claude and the mock GitHub world (#2141)."""

from __future__ import annotations

from datetime import datetime, timedelta, timezone

import pytest

from tests.harness.harness import Harness


def _removed(reason: str = "MANUAL") -> dict:
    """A queue snapshot whose entry is gone with a *readable* removal (see #2266).

    An ejection is only proven by a removal the watcher can read -- the timeline's
    last node carries the reason and a timestamp at or after the watch started --
    and the same removal has to be observed twice. Until #2266 a bare empty
    ``{}`` sample stood in for that, so this case kept passing while the waiter
    had already stopped calling it an ejection.
    """
    stamp = (datetime.now(timezone.utc) + timedelta(hours=1)).isoformat().replace("+00:00", "Z")
    return {"state": "OPEN", "removed": {"reason": reason, "createdAt": stamp}}


REMOVED_BY_QUEUE = _removed()


@pytest.mark.parametrize(("sequence", "message", "error"), [
    ([{"entry": "AWAITING_CHECKS", "position": 4},
      {"entry": "MERGEABLE", "position": 1}, {"state": "MERGED"}], "MERGED", False),
    ([{"entry": "AWAITING_CHECKS", "position": 4},
      REMOVED_BY_QUEUE, REMOVED_BY_QUEUE], "EJECTED", True),
    ([{"entry": "UNMERGEABLE", "position": 1}], "UNMERGEABLE", True),
    ([{"entry": "AWAITING_CHECKS", "position": 1}], "TIMEOUT", True),
])
def test_agent_waits_for_merge(harness: Harness, sequence: list[dict], message: str, error: bool):
    harness.write_gh_state({"repo": "o/r", "issues": {}, "prs": [
        {"number": 101, "state": "OPEN", "queue_sequence": sequence},
    ]})
    timeout = 2 if message == "TIMEOUT" else 30
    command = ("clud tool run github/pr_merge_watch.py -- 101 --repo o/r --until merged "
               f"--interval 0 --timeout {timeout} --no-cancel")
    script = {"default_text": "DONE", "roles": [{"name": "main", "steps": [
        {"tool_use": {"name": "Bash", "input": {"command": command}}},
        {"text": "DONE"},
    ]}]}
    result = harness.run("Wait for the pull request to merge.", script)
    assert result.returncode == 0, result.stderr
    results = [item for request in result.requests for item in request.get("tool_results", [])]
    matching = [item for item in results if message in str(item.get("content", ""))]
    assert matching, results
    assert matching[0]["is_error"] is error
    if message == "MERGED":
        assert "position=4 state=AWAITING_CHECKS" in matching[0]["content"]
        assert "position=1 state=MERGEABLE" in matching[0]["content"]
    calls = harness.read_gh_state()["calls"]
    assert not any("mutation" in arg for call in calls for arg in call)
