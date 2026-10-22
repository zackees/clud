"""Observe native check-result and structured-report hooks before readiness wiring."""
from __future__ import annotations

import json
import shlex
import sys
from pathlib import Path
from typing import Any

import pytest

from tests.harness.harness import Harness
from tests.harness.test_grind import _after, _bash, _no_notes, _structured


def record_outcomes(h: Harness, monkeypatch: pytest.MonkeyPatch, output: Path) -> None:
    original = h.settings
    recorder = Path(__file__).with_name("record_hook.py")
    command = " ".join(shlex.quote(str(arg)) for arg in (sys.executable, recorder, output))

    def settings(**kwargs: Any) -> Path:
        path = original(**kwargs)
        data = json.loads(path.read_text())
        for event in ("PostToolUse", "PostToolUseFailure"):
            data["hooks"][event] = [{
                "matcher": "*", "hooks": [{"type": "command", "command": command}],
            }]
        path.write_text(json.dumps(data))
        return path

    monkeypatch.setattr(h, "settings", settings)


@pytest.mark.parametrize("check_succeeds", [True, False])
def test_native_gate_evidence_hook_capabilities(
    harness: Harness, monkeypatch: pytest.MonkeyPatch, check_succeeds: bool,
) -> None:
    h = harness
    (h.config / "agents").mkdir(exist_ok=True)
    (h.config / "workflows").mkdir(exist_ok=True)
    (h.config / "agents" / "gate-evidence-probe.md").write_text(
        "---\nname: gate-evidence-probe\ndescription: Fixture evidence observer\n"
        "tools: Bash\n---\nGATE_EVIDENCE_OBSERVER_ROLE\n"
    )
    (h.config / "workflows" / "gate-evidence-probe.js").write_text(
        "export const meta = {name: 'gate-evidence-probe', description: 'Fixture evidence'}\n"
        "return await agent('Observe one check', {agentType: 'gate-evidence-probe', "
        "label: 'gate:evidence:0', schema: {type: 'object', required: ['ready'], "
        "properties: {ready: {type: 'boolean'}}}})\n"
    )
    output = h.logs / "gate-outcomes.jsonl"
    record_outcomes(h, monkeypatch, output)
    command = "echo GATE_EVIDENCE_SUCCESS" if check_succeeds else "false"
    script = {"roles": [
        {"name": "observer", "match": "GATE_EVIDENCE_OBSERVER_ROLE", "steps": [
            _bash(command),
            # Deliberately claim readiness after failure too: this measures
            # whether hooks expose the report and the actual command result.
            _after({"is_error": not check_succeeds}, _structured({"ready": True})),
        ]},
        {"name": "main", "steps": [
            {"tool_use": {"name": "Workflow", "input": {
                "name": "gate-evidence-probe", "args": {},
            }}},
            {"text": "EVIDENCE_PROBE_STARTED"},
        ]},
    ]}
    result = h.run("Observe the native evidence hooks once.", script)
    assert result.returncode == 0, result.stderr + result.stdout[-3000:]
    _no_notes(result)
    assert any(row.get("agent_type") == "gate-evidence-probe"
               and row.get("tool_name") == "Bash" for row in result.hooks)
    outcomes = [json.loads(line) for line in output.read_text().splitlines()]
    observed = [row for row in [*result.hooks, *outcomes]
                if row.get("agent_type") == "gate-evidence-probe"]
    print("NATIVE_GATE_EVIDENCE_CAPABILITIES", json.dumps({
        "check_succeeds": check_succeeds, "observed": observed,
    }, sort_keys=True))


def test_native_workflow_wait_capabilities(harness: Harness) -> None:
    h = harness
    (h.config / "workflows").mkdir(exist_ok=True)
    (h.config / "agents").mkdir(exist_ok=True)
    (h.config / "agents" / "wait-capability-probe.md").write_text(
        "---\nname: wait-capability-probe\ndescription: Fixture capability observer\n"
        "tools: []\n---\nWAIT_CAPABILITY_OBSERVER_ROLE\n"
    )
    (h.config / "workflows" / "wait-capability-probe.js").write_text(
        "export const meta = {name: 'wait-capability-probe', description: 'Wait capabilities'}\n"
        "const capabilities = {setTimeout: typeof setTimeout, sleep: typeof sleep, "
        "wait: typeof wait, Promise: typeof Promise};\n"
        "if (typeof setTimeout === 'function') {\n"
        "  const started = Date.now();\n"
        "  await new Promise(resolve => setTimeout(resolve, 20));\n"
        "  capabilities.timerElapsedMs = Date.now() - started;\n"
        "}\n"
        "return await agent('NATIVE_WAIT_CAPABILITIES ' + JSON.stringify(capabilities), "
        "{agentType: 'wait-capability-probe', label: 'wait-capabilities'});\n"
    )
    result = h.run("Observe native workflow wait capabilities without waiting.", {"roles": [
        {"name": "wait-probe", "match": "WAIT_CAPABILITY_OBSERVER_ROLE", "steps": [
            {"text": "WAIT_CAPABILITIES_OBSERVED"},
        ]},
        {"name": "main", "steps": [
            {"tool_use": {"name": "Workflow", "input": {
                "name": "wait-capability-probe", "args": {},
            }}},
            {"text": "WAIT_CAPABILITY_PROBE_DONE"},
        ]},
    ]})
    assert result.returncode == 0, result.stderr + result.stdout[-3000:]
    _no_notes(result)
    messages = result.first_request("wait-probe")["messages"]
    texts = []
    for message in messages:
        content = message.get("content", [])
        if isinstance(content, str):
            texts.append(content)
        else:
            texts.extend(block.get("text", "") for block in content if isinstance(block, dict))
    marker = "NATIVE_WAIT_CAPABILITIES "
    observed = next(text.split(marker, 1)[1] for text in texts if marker in text)
    capabilities, _ = json.JSONDecoder().raw_decode(observed)
    assert capabilities["setTimeout"] in ("function", "undefined"), capabilities
    if capabilities["setTimeout"] == "function":
        assert capabilities["timerElapsedMs"] >= 10, capabilities
    print("NATIVE_WORKFLOW_WAIT_CAPABILITIES", json.dumps(capabilities, sort_keys=True))
