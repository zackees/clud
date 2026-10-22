"""Classify multi-agent native cache replay independently of grind's roles."""
from __future__ import annotations

from collections import Counter

import pytest

from tests.harness.harness import Harness
from tests.harness.test_grind import _after, _bash, _no_notes, _structured
from tests.harness.test_orchestration_resume import capture_resume_input, native_resume_steps
from tests.harness.workflow_results import workflow_results


@pytest.mark.parametrize("parallel_mode", [False, True])
@pytest.mark.parametrize("native_writes", [False, True])
def test_native_cache_replays_every_stage_of_two_chains(
    harness: Harness, monkeypatch, parallel_mode: bool, native_writes: bool,
) -> None:
    h = harness
    captured = capture_resume_input(h, monkeypatch)
    (h.config / "agents").mkdir(exist_ok=True)
    (h.config / "workflows").mkdir(exist_ok=True)
    (h.config / "agents" / "cache-chain-probe.md").write_text(
        "---\nname: cache-chain-probe\ndescription: Native chain cache probe\ntools: [Read, Write, Bash]\n---\n"
        "CACHE_CHAIN_PROBE_ROLE\n"
    )
    run = ("return await parallel([() => chain('a'), () => chain('d')]);" if parallel_mode
           else "return [await chain('a'), await chain('d')];")
    (h.config / "workflows" / "cache-chain-probe.js").write_text(
        "export const meta = {name: 'cache-chain-probe', description: 'Native chain cache', "
        "phases: [0,1,2,3].map(i => ({title: 'Stage ' + i}))};\n"
        "const schema = {type: 'object', required: ['observed'], "
        "properties: {observed: {type: 'string'}}};\n"
        "async function chain(id) {\n"
        "  const observed = [];\n"
        "  for (let i = 0; i < 4; i++) {\n"
        "    phase('Stage ' + i);\n"
        "    observed.push(await agent('CACHE_CHAIN_' + id + i, {\n"
        "      agentType: 'cache-chain-probe', label: id + i, phase: 'Stage ' + i, schema}));\n"
        "  }\n"
        "  return observed;\n"
        "}\n" + run + "\n"
    )
    roles = []
    expected = []
    for goal in ("a", "d"):
        for stage in range(4):
            label = f"{goal}{stage}"
            expected.append(label)
            steps = [_bash(f"printf '{label},' >> cache-chain.txt")]
            if native_writes:
                steps = [{"tool_use": {"name": "Write", "input": {
                    "file_path": str(h.repo / f"cache-{label}.txt"), "content": label,
                }}}, _after({"is_error": False}, steps[0])]
            steps.append(_after({"is_error": False}, _structured({"observed": label})))
            roles.append({"name": label, "match": "CACHE_CHAIN_PROBE_ROLE",
                          "match_prompt": f"CACHE_CHAIN_{label}", "steps": steps})
    roles.extend([
        {"name": "main", "match_prompt": "</task-notification>",
         "steps": native_resume_steps(captured, False)},
        {"name": "main", "steps": [
            {"tool_use": {"name": "Workflow", "input": {
                "name": "cache-chain-probe", "args": {"gateEpochs": {"b": 1, "c": 1}},
            }}},
            {"text": "CHAIN_PROBE_STARTED"},
        ]},
    ])
    result = h.run("Observe native cache replay across each stage of both chains.", {"roles": roles})
    assert result.returncode == 0, result.stderr + result.stdout[-3000:]
    _no_notes(result)
    outputs = workflow_results(result)
    assert len(outputs) == 2, outputs
    wanted = [[{"observed": f"{goal}{stage}"} for stage in range(4)] for goal in ("a", "d")]
    assert outputs == [wanted, wanted], outputs
    observed = Counter((h.repo / "cache-chain.txt").read_text().rstrip(",").split(","))
    assert observed == Counter(expected), observed
