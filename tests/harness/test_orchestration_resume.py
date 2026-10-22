"""Resume the real native Workflow cache without redoing completed branches."""
from __future__ import annotations

import difflib
import json
import shlex
import sys
from pathlib import Path
from typing import Any

import pytest

from tests.harness.harness import Harness
from tests.harness.test_grind import (
    _after,
    _bash,
    _goal_roles,
    _goals,
    _merged,
    _no_notes,
    _script,
    _structured,
)
from tests.harness.test_grind_gate_binding import install_outcome_guard
from tests.harness.workflow_results import workflow_notices, workflow_results


def capture_resume_input(h: Harness, monkeypatch: pytest.MonkeyPatch) -> Path:
    install_outcome_guard(h, monkeypatch)
    epochs = h.logs / "next-gate-epochs.json"
    epochs.write_text(json.dumps({"b": 1, "c": 1}))
    output = h.logs / "native-resume-input.json"
    original = h.settings
    recorder = Path(__file__).with_name("capture_workflow_resume.py")
    command = shlex.join([sys.executable, str(recorder), str(epochs), str(output)])

    def settings(**kwargs: Any) -> Path:
        path = original(**kwargs)
        data = json.loads(path.read_text())
        data["hooks"]["PostToolUse"].append({"matcher": "Workflow", "hooks": [{
            "type": "command", "command": command,
        }]})
        path.write_text(json.dumps(data))
        return path

    monkeypatch.setattr(h, "settings", settings)
    return output


def gate_role(name: str, goal: str, tag: str, ready: bool, epoch: int | None = None) -> dict:
    role = {"name": name, "match": "You are the /grind gate checker", "match_prompt": [
        f"Goal {goal}:",
    ], "steps": [
        _bash(f"gh release view {tag} --repo o/r"),
        _after({"is_error": not ready}, _structured({"ready": ready, "evidence": tag})),
    ]}
    if epoch is not None:
        role["match_prompt"].append(f"Observation: {epoch}/1.")
    return role



def native_resume_steps(captured: Path, scheduled: bool) -> list[dict[str, Any]]:
    steps = [{"text": "unused"}, {"text": "unused"}]
    if scheduled:
        steps.extend([
            {"tool_use": {"name": "CronCreate", "input": {
                "cron": "* * * * *", "recurring": True,
                "prompt": 'WAKE_DEFERRED_GRAPH remaining_goals=["b","c"]',
            }}},
            _after({"is_error": False}, {"text": "GRAPH_RESUMPTION_ARMED"}),
        ])
    steps.extend([
        {"tool_use": {"name": "Workflow", "input_file": str(captured)}},
        _after({"is_error": False}, {"text": "NATIVE_RESUME_STARTED"}),
        {"text": "NATIVE_GRAPH_RESUME_COMPLETE"},
    ])
    return steps


@pytest.mark.timeout(240)
@pytest.mark.parametrize("scheduled", [False, True])
def test_native_resume_only_reobserves_deferred_branch(
    harness: Harness, monkeypatch, scheduled: bool,
) -> None:
    h = harness
    graph = {"session_repo": "o/r", "session_root": str(h.repo), "goals": [
        {"id": "a", "repo": "o/r", "main": "main", "blockers": []},
        {"id": "b", "repo": "o/r", "main": "main", "blockers": [
            {"goal": "a", "when": "released", "check": "gh release view v-pending --repo o/r"},
        ]},
        {"id": "c", "repo": "o/r", "main": "main", "blockers": [{"goal": "b", "when": "merged"}]},
        {"id": "d", "repo": "o/r", "main": "main", "blockers": []},
        {"id": "e", "repo": "o/r", "main": "main", "blockers": [
            {"goal": "d", "when": "released", "check": "gh release view v-ready --repo o/r"},
        ]},
    ], "proposed_goals": [], "risks": []}
    state = h.read_gh_state()
    state["releases"] = {"v-ready": {"publishedAt": "2026-10-22T00:00:00Z"}}
    state["release_after_views"] = {"v-pending": {
        "after": 1, "release": {"publishedAt": "2026-10-22T00:00:00Z"},
    }}
    h.write_gh_state(state)
    h.write_run_facts({"phase": "execute", "orchestration": graph})
    captured = capture_resume_input(h, monkeypatch)
    roles = [role for goal in ("a", "b", "c", "d", "e") for role in _goal_roles(h, goal)]
    roles.extend([
        gate_role("gate:b:resume", "b", "v-pending", True, 1),
        gate_role("gate:b:initial", "b", "v-pending", False),
        gate_role("gate:e", "e", "v-ready", True),
    ])
    script = _script(h, "parallel", _goals("a", "b", "c", "d", "e"), roles,
                     orchestration=graph, gateAttempts=1)
    script["roles"].insert(-1, {"name": "main", "match_prompt": "</task-notification>",
                               "steps": native_resume_steps(captured, scheduled)})
    if scheduled:
        result = h.run_clud_interactive(
            "Resume the deferred graph through a native scheduled wakeup.", script,
            model="nvidia/nemotron-3-ultra-550b-a55b:free",
            finished="NATIVE_GRAPH_RESUME_COMPLETE", timeout=180,
        )
    else:
        result = h.run("Resume only deferred publication work using the native Workflow cache.",
                       script, timeout=180)
    assert result.returncode == 0, result.stderr + result.stdout[-3000:]
    if any(row.get("note") for row in result.requests):
        print("NATIVE_RESUME_REFERENCE", captured.read_text() if captured.exists() else "missing")
        for role in ("planner:a", "worker:a", "reviewer:a", "integrator:a"):
            requests = [row for row in result.requests
                        if row["role"] == role and row.get("turn") == 0]
            if len(requests) > 1:
                first = json.dumps(requests[0]["messages"], indent=1).splitlines()
                last = json.dumps(requests[-1]["messages"], indent=1).splitlines()
                print("NATIVE_RESUME_PROMPT_DIFF", role,
                      "\n".join(list(difflib.unified_diff(first, last))[:80]))
        for journal in list(h.config.glob("projects/*/*/subagents/workflows/*/journal.jsonl"))[:4]:
            print("NATIVE_RESUME_JOURNAL", journal.name, journal.read_text()[:20000])
    _no_notes(result)
    outputs = workflow_results(result)
    assert len(outputs) == 2, outputs
    assert outputs[0]["resume"]["remaining_goals"] == ["b", "c"], outputs[0]
    assert "resume" not in outputs[1], outputs[1]
    reference = json.loads(captured.read_text())
    assert reference["resumeFromRunId"].startswith("wf_")
    resumed = [row["n"] for row in result.requests
               if row.get("step", {}).get("tool_use", {}).get("input_file") == str(captured)]
    assert len(resumed) == 1, resumed
    boundary = resumed[0]
    if scheduled:
        assert any(row.get("event") == "PostToolUse" and row.get("tool_name") == "CronCreate"
                   for row in result.hooks)
        resumed_request = next(row for row in result.requests if row["n"] == boundary)
        assert "WAKE_DEFERRED_GRAPH" in json.dumps(resumed_request["messages"][-1])
    assert _merged(h) == [f"grind/{goal}" for goal in ("a", "b", "c", "d", "e")]
    for row in result.requests:
        if row["role"] in ("gate:e", "planner:a", "worker:a", "reviewer:a", "integrator:a",
                           "lander:a:0", "planner:d", "worker:d", "reviewer:d", "integrator:d",
                           "lander:d:0", "planner:e", "worker:e", "reviewer:e", "integrator:e",
                           "lander:e:0"):
            assert row["n"] < boundary, row["role"]
    assert any(row["role"] == "gate:b:resume" and row["n"] > boundary for row in result.requests)
    assert any(row["role"] == "planner:b" and row["n"] > boundary for row in result.requests)
    assert any(row["role"] == "planner:c" and row["n"] > boundary for row in result.requests)


@pytest.mark.parametrize("orchestrated", [False, True])
@pytest.mark.parametrize("reason", ["no-overlap", "deferred-group"])
def test_stage_deferral_never_creates_publication_resume(
    harness: Harness, orchestrated: bool, reason: str,
) -> None:
    h = harness
    plan = {"stages": [{"stage": "feature", "group": "next", "children": ["b"]}],
            "rules": {"no_overlap": "bugs_only"}, "deferred_groups": []}
    if reason == "no-overlap":
        plan["waiting_on_pr"] = 90
    else:
        plan["deferred_groups"] = [{"group": "next", "children": ["b"]}]
    args = {"plan": plan, "plan_url": "https://github.com/o/r/issues/1#issuecomment-1"}
    if orchestrated:
        args["orchestration"] = {"session_repo": "o/r", "session_root": str(h.repo),
                                 "goals": [{"id": "b", "repo": "o/r", "main": "main",
                                            "blockers": []}]}
    before = h.git("status", "--porcelain")
    result = h.run("Preserve the stage deferral without scheduling publication checks.",
                   _script(h, "sequential", _goals("b"), [], **args))
    assert result.returncode == 0, result.stderr + result.stdout[-3000:]
    _no_notes(result)
    assert {row["role"] for row in result.requests} == {"main"}
    outputs = workflow_results(result)
    assert len(outputs) == 1, outputs
    assert outputs[0]["goals"][0]["status"] == "deferred", outputs
    assert "resume" not in outputs[0], outputs
    assert h.git("status", "--porcelain") == before


@pytest.mark.parametrize("change_args", [False, True])
@pytest.mark.parametrize("structured", [False, True])
def test_native_resume_replays_one_unchanged_agent(
    harness: Harness, monkeypatch, change_args: bool, structured: bool,
) -> None:
    """Separate agent-cache behavior from parallelism and argument changes."""
    h = harness
    captured = capture_resume_input(h, monkeypatch)
    (h.config / "workflows").mkdir(exist_ok=True)
    (h.config / "agents").mkdir(exist_ok=True)
    (h.config / "agents" / "resume-cache-probe.md").write_text(
        "---\nname: resume-cache-probe\ndescription: Native cache probe\ntools: [Bash]\n---\n"
        "NATIVE_CACHE_PROBE_ROLE\n"
    )
    options = {"agentType": "resume-cache-probe", "label": "cache-probe"}
    if structured:
        options["schema"] = {"type": "object", "required": ["observed"], "properties": {
            "observed": {"type": "string"},
        }}
    answer = "observed.observed" if structured else "observed"
    (h.config / "workflows" / "resume-cache-probe.js").write_text(
        "export const meta = {name: 'resume-cache-probe', description: 'Native cache probe'};\n"
        "const observed = await agent('Write the cache probe once.', "
        + json.dumps(options) + ");\nreturn " + answer + ";\n"
    )
    report = (_structured({"observed": "CACHE_OBSERVATION_COMPLETE"}) if structured
              else {"text": "CACHE_OBSERVATION_COMPLETE"})
    resume = {"tool_use": {"name": "Workflow", "input_file": str(captured)}}
    script = {"roles": [
        {"name": "cache-probe", "match": "NATIVE_CACHE_PROBE_ROLE", "steps": [
            _bash("printf observed >> cache-probe.txt"),
            _after({"is_error": False}, report),
        ]},
        {"name": "main", "match_prompt": "</task-notification>", "steps": [
            {"text": "unused"}, {"text": "unused"}, resume,
            _after({"is_error": False}, {"text": "CACHE_RESUME_STARTED"}),
        ]},
        {"name": "main", "steps": [
            {"tool_use": {"name": "Workflow", "input": {
                "name": "resume-cache-probe",
                "args": {} if change_args else {"gateEpochs": {"b": 1, "c": 1}},
            }}},
            {"text": "CACHE_PROBE_STARTED"},
        ]},
    ]}
    result = h.run("Observe the native unchanged-agent cache contract.", script)
    assert result.returncode == 0, result.stderr + result.stdout[-3000:]
    _no_notes(result)
    calls = [row for row in result.hooks if row.get("event") == "PreToolUse"
             and row.get("tool_name") == "Bash"
             and row.get("tool_input", {}).get("command") == "printf observed >> cache-probe.txt"]
    resumed = [row for row in result.requests if row.get("step", {}).get("tool_use", {}).get(
        "input_file") == str(captured)]
    assert len(resumed) == 1, resumed
    assert "CACHE_RESUME_STARTED" in result.stdout
    notices = workflow_notices(result)
    assert len(notices) == 2, notices
    for notice in notices.values():
        assert "<status>completed</status>" in notice, notice
        assert "<result>" in notice, notice
        assert "CACHE_OBSERVATION_COMPLETE" in notice, notice
    assert len(calls) == 1, {"calls": calls, "reference": captured.read_text()}
    assert (h.repo / "cache-probe.txt").read_text() == "observed"
