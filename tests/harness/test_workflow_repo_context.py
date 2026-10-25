"""Measure real workflow repository context before implementing cross-repo runs."""
from __future__ import annotations

import json
import shlex
import sys
from pathlib import Path

import pytest

from tests.harness.harness import Harness

ROLE = "REPO_CONTEXT_AGENT_ROLE"


def project_source(h: Harness, root: Path, name: str) -> None:
    directory = root / ".claude"
    directory.mkdir(exist_ok=True)
    (root / "CLAUDE.md").write_text(f"REPO_CONTEXT_INSTRUCTIONS_{name.upper()}\n")
    hook = directory / f"{name}-hook.py"
    log = str(h.logs / "project-hooks.jsonl")
    hook.write_text(
        "import json, os, sys\n"
        "event = json.load(sys.stdin)\n"
        f"with open({log!r}, 'a') as stream:\n"
        f" stream.write(json.dumps({{'source': {name!r}, 'cwd': os.getcwd(), "
        "'tool': event.get('tool_name')}) + '\\n')\n"
    )
    command = f"{shlex.quote(sys.executable)} .claude/{name}-hook.py"
    settings = {
        "env": {"REPO_CONTEXT_CANARY": name},
        "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [
            {"type": "command", "command": command},
        ]}]},
    }
    (directory / "settings.json").write_text(json.dumps(settings))


def prepare_probe(h: Harness) -> tuple[Path, str, Path]:
    sister = h.root / "sister repo"
    sister.mkdir()
    h.git("init", "-q", "-b", "master", cwd=sister)
    project_source(h, h.repo, "primary")
    project_source(h, sister, "sister")
    agents = h.config / "agents"
    agents.mkdir(exist_ok=True)
    (agents / "repo-context-probe.md").write_text(
        "---\nname: repo-context-probe\ndescription: Fixture context probe\ntools: Bash\n---\n"
        + ROLE + "\nRun the one requested harmless identity command.\n"
    )
    workflows = h.config / "workflows"
    workflows.mkdir(exist_ok=True)
    (workflows / "repo-context-probe.js").write_text(
        "export const meta = {name: 'repo-context-probe', description: 'Fixture context probe'}\n"
        "const options = {agentType: 'repo-context-probe', label: 'context'}\n"
        "if (args.agentCwd) options.cwd = args.target\n"
        "return await agent('Run the identity command once.', options)\n"
    )
    observed = h.logs / "identity.json"
    script = h.root / "identity.py"
    script.write_text(
        "import json, os\n"
        f"with open({str(observed)!r}, 'w') as stream:\n"
        " json.dump({'cwd': os.getcwd(), 'project': os.getenv('REPO_CONTEXT_CANARY'), "
        "'agent_env': os.getenv('CLAUDE_CODE_AGENT_ID'), "
        "'session_env': os.getenv('CLAUDE_CODE_SESSION_ID')}, stream)\n"
    )
    command = f"{shlex.quote(sys.executable)} {shlex.quote(str(script))}"
    return sister, command, observed


@pytest.mark.parametrize("selection", ["default", "agent", "workflow", "session"])
def test_workflow_repo_context(harness: Harness, selection: str) -> None:
    h = harness
    sister, command, observed = prepare_probe(h)
    call = {"name": "repo-context-probe", "args": {
        "target": str(sister), "agentCwd": selection == "agent",
    }}
    if selection == "workflow":
        call["cwd"] = str(sister)
    script = {"default_text": "DONE", "roles": [
        {"name": "probe", "match": ROLE, "steps": [
            {"tool_use": {"name": "Bash", "input": {"command": command}}},
            {"text": "CONTEXT_PROBE_DONE"},
        ]},
        {"name": "main", "steps": [
            {"tool_use": {"name": "Workflow", "input": call}},
            {"text": "CONTEXT_PROBE_STARTED"},
        ]},
    ]}
    before = [h.git("status", "--porcelain", cwd=root) for root in (h.repo, sister)]
    result = h.run(
        "Run the fixture workflow once.", script, cmd_scan=False, timeout=120,
        cwd=sister if selection == "session" else None,
    )
    assert result.returncode == 0, result.stderr + result.stdout[-3000:]
    errors = [block for row in result.requests for block in row.get("tool_results", [])
              if block["is_error"]]
    if selection == "workflow":
        # Capability classification, not cross-repo acceptance: the pinned
        # runtime rejects this input before spawning any workflow child.
        assert any("unexpected parameter `cwd`" in row["content"] for row in errors), errors
        assert not observed.exists()
        assert not any(row["role"] == "probe" for row in result.requests)
        assert [h.git("status", "--porcelain", cwd=root) for root in (h.repo, sister)] == before
        return
    assert observed.is_file(), errors
    identity = json.loads(observed.read_text())
    # The unknown agent option is silently ignored. Pin that limitation so
    # orchestration cannot mistake successful execution for repo isolation.
    expected = sister if selection == "session" else h.repo
    assert identity["cwd"] == str(expected), {"selection": selection, "observed": identity}
    name = "sister" if selection == "session" else "primary"
    assert identity["project"] == name, identity
    requests = [row for row in result.requests if row["role"] == "probe"]
    assert f"REPO_CONTEXT_INSTRUCTIONS_{name.upper()}" in json.dumps(requests)
    hooks = [row for row in result.hooks if row.get("agent_type") == "repo-context-probe"]
    assert hooks, result.hooks
    assert {row["cwd"] for row in hooks} == {str(expected)}
    source_log = h.logs / "project-hooks.jsonl"
    assert source_log.is_file(), result.stdout[-3000:]
    sources = [json.loads(line) for line in source_log.read_text().splitlines()]
    assert sources == [{"source": name, "cwd": str(expected), "tool": "Bash"}]
    assert [h.git("status", "--porcelain", cwd=root) for root in (h.repo, sister)] == before


def test_workflow_repo_context_authoring_reference(harness: Harness) -> None:
    script = {"default_text": "DONE", "roles": [{"name": "main", "steps": [
        {"tool_use": {"name": "Skill", "input": {"skill": "workflow-authoring"}}},
        {"text": "REFERENCE_LOADED"},
    ]}]}
    result = harness.run("Load the bundled workflow authoring reference.", script, cmd_scan=False)
    assert result.returncode == 0, result.stderr
    results = [block for row in result.requests for block in row.get("tool_results", [])]
    assert results, result.requests
    assert not any(block["is_error"] for block in results), results
    texts = []
    for row in result.requests:
        for message in row["messages"]:
            if message.get("role") != "user":
                continue
            content = message.get("content", [])
            if isinstance(content, str):
                texts.append(content)
            else:
                texts.extend(block["text"] for block in content if block.get("type") == "text")
    reference = "\n".join(texts)
    assert "agent(" in reference, reference
    (harness.logs / "workflow-authoring.txt").write_text(reference)
    signatures = [line for line in reference.splitlines()
                  if "agent(prompt" in line or "opts.cwd" in line or "opts.repo" in line]
    schemas = [tool for row in result.requests for tool in row.get("orchestration_tools", [])]
    assert any(tool["name"] == "Workflow" for tool in schemas), schemas
    print("WORKFLOW_AUTHORING_SIGNATURES", json.dumps(signatures)[:2500])
    print("WORKFLOW_ORCHESTRATION_SCHEMAS", json.dumps(schemas[:2])[:6000])
