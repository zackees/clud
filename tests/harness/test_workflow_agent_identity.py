"""Measure agent-to-task metadata before implementing exact gate capabilities."""
from __future__ import annotations

import json
import shlex
import sys
from pathlib import Path
from typing import Any

import pytest

from tests.harness.harness import Harness
from tests.harness.test_grind import _bash, _no_notes

ROLE = "WORKFLOW_IDENTITY_PROBE_ROLE"
AGENT = "workflow-identity-probe"


def instrument_settings(h: Harness, monkeypatch: pytest.MonkeyPatch, output: Path) -> None:
    original = h.settings
    recorder = Path(__file__).with_name("record_workflow_identity.py")
    command = " ".join(shlex.quote(str(arg)) for arg in (
        sys.executable, recorder, h.config, output,
    ))

    def settings(**kwargs: Any) -> Path:
        path = original(**kwargs)
        data = json.loads(path.read_text())
        data["hooks"]["PreToolUse"].append({
            "matcher": "Bash", "hooks": [{"type": "command", "command": command}],
        })
        path.write_text(json.dumps(data))
        return path

    monkeypatch.setattr(h, "settings", settings)


@pytest.mark.parametrize("selection", ["primary", "sister"])
def test_workflow_identity_metadata_precedes_first_tool(
    harness: Harness, monkeypatch: pytest.MonkeyPatch, selection: str,
) -> None:
    h = harness
    target = h.repo
    if selection == "sister":
        target = h.root / "sister repo"
        target.mkdir()
        h.git("init", "-q", "-b", "master", cwd=target)
    (h.config / "agents").mkdir(exist_ok=True)
    (h.config / "workflows").mkdir(exist_ok=True)
    (h.config / "agents" / f"{AGENT}.md").write_text(
        f"---\nname: {AGENT}\ndescription: Fixture identity probe\ntools: Bash\n---\n{ROLE}\n"
    )
    (h.config / "workflows" / f"{AGENT}.js").write_text(
        f"export const meta = {{name: '{AGENT}', description: 'Fixture identity probe'}}\n"
        "return await Promise.all(['left', 'right'].map(side => "
        f"agent('Inspect identity ' + side, {{agentType: '{AGENT}', label: 'gate:' + side}})))\n"
    )
    output = h.logs / "workflow-identities.jsonl"
    instrument_settings(h, monkeypatch, output)
    commands = {side: f"echo WORKFLOW_IDENTITY_{side.upper()}" for side in ("left", "right")}
    roles = [{"name": side, "match": ROLE, "match_prompt": f"Inspect identity {side}",
              "steps": [_bash(command), {"text": f"IDENTITY_{side}_DONE"}]}
             for side, command in commands.items()]
    roles.append({"name": "main", "steps": [
        {"tool_use": {"name": "Workflow", "input": {"name": AGENT, "args": {}}}},
        {"text": "IDENTITY_PROBE_STARTED"},
    ]})
    before = h.git("status", "--porcelain", cwd=target)
    result = h.run("Run both labeled identity tasks once.", {"roles": roles}, cwd=target)
    assert result.returncode == 0, result.stderr + result.stdout[-3000:]
    _no_notes(result)
    rows = [json.loads(line) for line in output.read_text().splitlines()]
    print("WORKFLOW_IDENTITY_BINDINGS", json.dumps(rows, sort_keys=True))
    assert len(rows) == 2, rows
    assert len({row["agent_id"] for row in rows}) == 2, rows
    expected = {command: f"gate:{side}" for side, command in commands.items()}
    assert {row["command"] for row in rows} == set(expected), rows
    for row in rows:
        assert row["session_id"] == h.session_id, rows
        assert row["agent_type"] == AGENT, rows
        assert len(row["metadata"]) == 1, rows
        assert row["metadata"][0]["agent_type"] == AGENT, rows
        assert row["metadata"][0]["label"] == expected[row["command"]], rows
    assert h.git("status", "--porcelain", cwd=target) == before
