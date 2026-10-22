"""Native gate hooks must bind one approved check to one actual workflow agent."""
from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from tests.harness.harness import EXE, Harness
from tests.harness.test_grind import _after, _bash, _no_notes, _structured
from tests.harness.test_workflow_gate_evidence import record_outcomes

ROLE = "GATE_BINDING_CAPABILITY_FIXTURE"


def install_probe(h: Harness) -> None:
    (h.config / "agents").mkdir(exist_ok=True)
    (h.config / "workflows").mkdir(exist_ok=True)
    (h.config / "agents" / "grind-gate.md").write_text(
        "---\nname: grind-gate\ndescription: Fixture gate role\ntools: Bash\n---\n"
        + ROLE + "\n"
    )
    (h.config / "workflows" / "gate-binding-probe.js").write_text(
        "export const meta = {name: 'gate-binding-probe', description: 'Fixture gate binding'}\n"
        "return await Promise.all(['left', 'right'].map((side, index) => "
        "agent('Inspect gate ' + side, {agentType: 'grind-gate', "
        "label: 'gate:downstream:' + index})))\n"
    )


def install_outcome_guard(h: Harness, monkeypatch: pytest.MonkeyPatch) -> Path:
    output = h.logs / "gate-native-outcomes.jsonl"
    record_outcomes(h, monkeypatch, output)
    original = h.settings

    def settings(**kwargs: Any) -> Path:
        path = original(**kwargs)
        data = json.loads(path.read_text())
        scanner = h.clud.parent / ("clud-cmd-scan" + EXE)
        for event in ("PostToolUse", "PostToolUseFailure"):
            data["hooks"][event].append({"matcher": "Bash", "hooks": [{
                "type": "command", "command": f'"{scanner}" --event {event}',
            }]})
        path.write_text(json.dumps(data))
        return path

    monkeypatch.setattr(h, "settings", settings)
    return output


@pytest.mark.parametrize("selection", ["primary", "sister"])
@pytest.mark.parametrize("facts_state", ["approved", "missing", "plan"])
def test_native_gate_can_only_execute_its_bound_check(
    harness: Harness, selection: str, facts_state: str,
) -> None:
    h = harness
    target = h.repo
    if selection == "sister":
        target = h.root / "sister repo"
        target.mkdir()
        h.git("init", "-q", "-b", "master", cwd=target)
    install_probe(h)
    commands = {side: f"echo GATE_CHECK_{side.upper()}" for side in ("left", "right")}
    if facts_state != "missing":
        h.write_run_facts({
            "phase": "plan" if facts_state == "plan" else "execute",
            "orchestration": {
                "session_repo": "o/r", "session_root": str(target),
                "goals": [{"id": "downstream", "repo": "o/r", "blockers": [
                    {"goal": side, "when": "released", "check": command}
                    for side, command in commands.items()
                ]}],
            },
        })
    roles = []
    for side, command in commands.items():
        other = commands["right" if side == "left" else "left"]
        roles.append({"name": side, "match": ROLE, "match_prompt": f"Inspect gate {side}",
                      "steps": [
                          _bash(other),
                          _after({"is_error": True}, _bash(command)),
                          _after({"is_error": facts_state != "approved"},
                                 _bash(command + " > gate-write-canary.txt")),
                          _after({"is_error": True}, {"text": "GATE_BINDING_DONE"}),
                      ]})
    roles.append({"name": "main", "steps": [
        {"tool_use": {"name": "Workflow", "input": {"name": "gate-binding-probe", "args": {}}}},
        {"text": "GATE_BINDING_STARTED"},
    ]})
    before = h.git("status", "--porcelain", cwd=target)
    result = h.run("Run the labeled gate capability probes once.", {"roles": roles}, cwd=target)
    assert result.returncode == 0, result.stderr + result.stdout[-3000:]
    _no_notes(result)
    calls = [row for row in result.hooks if row.get("agent_type") == "grind-gate"
             and row.get("event") == "PreToolUse" and row.get("tool_name") == "Bash"]
    assert len(calls) == 6, calls
    assert len({row["agent_id"] for row in calls}) == 2, calls
    assert not (target / "gate-write-canary.txt").exists()
    assert h.git("status", "--porcelain", cwd=target) == before


def test_native_gate_cannot_report_ready_without_executing_check(harness: Harness) -> None:
    h = harness
    install_probe(h)
    (h.config / "workflows" / "gate-binding-probe.js").write_text(
        "export const meta = {name: 'gate-binding-probe', description: 'Gate evidence'}\n"
        "return await agent('Inspect gate left', {agentType: 'grind-gate', "
        "label: 'gate:downstream:0', schema: {type: 'object', required: ['ready'], "
        "properties: {ready: {type: 'boolean'}}}})\n"
    )
    h.write_run_facts({"phase": "execute", "orchestration": {
        "session_repo": "o/r", "session_root": str(h.repo),
        "goals": [{"id": "downstream", "repo": "o/r", "blockers": [
            {"goal": "upstream", "when": "released", "check": "echo REQUIRED_CHECK"},
        ]}],
    }})
    result = h.run("Reject an unproved readiness claim.", {"roles": [
        {"name": "gate", "match": ROLE, "steps": [
            _structured({"ready": True}),
            _after({"is_error": True}, {"text": "UNPROVED_READY_REJECTED"}),
        ]},
        {"name": "main", "steps": [
            {"tool_use": {"name": "Workflow", "input": {
                "name": "gate-binding-probe", "args": {},
            }}},
            {"text": "GATE_REPORT_PROBE_DONE"},
        ]},
    ]})
    assert result.returncode == 0, result.stderr + result.stdout[-3000:]
    _no_notes(result)
    reports = [row for row in result.hooks if row.get("agent_type") == "grind-gate"
               and row.get("tool_name") == "StructuredOutput"]
    assert len(reports) == 1, reports
    assert any(row.get("role") == "gate"
               and row.get("step", {}).get("text") == "UNPROVED_READY_REJECTED"
               for row in result.requests), result.requests


@pytest.mark.parametrize("succeeds", [True, False])
def test_native_gate_readiness_matches_executed_outcome(
    harness: Harness, monkeypatch: pytest.MonkeyPatch, succeeds: bool,
) -> None:
    h = harness
    install_probe(h)
    (h.config / "workflows" / "gate-binding-probe.js").write_text(
        "export const meta = {name: 'gate-binding-probe', description: 'Gate outcome'}\n"
        "return await agent('Inspect gate left', {agentType: 'grind-gate', "
        "label: 'gate:downstream:0', schema: {type: 'object', required: ['ready'], "
        "properties: {ready: {type: 'boolean'}}}})\n"
    )
    command = "echo VERIFIED_NATIVE_GATE_SUCCESS" if succeeds else "test 1 = 2"
    h.write_run_facts({"phase": "execute", "orchestration": {
        "session_repo": "o/r", "session_root": str(h.repo),
        "goals": [{"id": "downstream", "repo": "o/r", "blockers": [
            {"goal": "upstream", "when": "released", "check": command},
        ]}],
    }})
    output = install_outcome_guard(h, monkeypatch)
    result = h.run("Check native gate readiness against execution.", {"roles": [
        {"name": "gate", "match": ROLE, "steps": [
            _bash(command),
            _after({"is_error": not succeeds}, _structured({"ready": True})),
            _after({"is_error": True}, _structured({"ready": False})),
        ]},
        {"name": "main", "steps": [
            {"tool_use": {"name": "Workflow", "input": {
                "name": "gate-binding-probe", "args": {},
            }}},
            {"text": "GATE_NATIVE_OUTCOME_DONE"},
        ]},
    ]})
    assert result.returncode == 0, result.stderr + result.stdout[-3000:]
    _no_notes(result)
    outcomes = [json.loads(line) for line in output.read_text().splitlines()]
    reports = [row for row in outcomes if row.get("agent_type") == "grind-gate"
               and row.get("tool_name") == "StructuredOutput"
               and row.get("event") == "PostToolUse"]
    assert [row["tool_input"]["ready"] for row in reports] == [succeeds], outcomes
