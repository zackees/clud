"""Native gate hooks must bind one approved check to one actual workflow agent."""
from __future__ import annotations

import pytest

from tests.harness.harness import Harness
from tests.harness.test_grind import _after, _bash, _no_notes, _structured

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
