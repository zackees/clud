"""Offline execution of grind's repository handoff contract (#1897)."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from tests import process

ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / "crates/clud-bin/assets/workflows/grind-run.js"
PROBE = ROOT / "tests/fixtures/grind_workflow_probe.mjs"


def probe(tmp_path: Path, **fields: object) -> dict:
    args = {
        "repo": str(tmp_path),
        "main": "main",
        "planOnly": True,
        "meta": "10",
        "goals": [{"id": "12", "title": "fix", "brief": "fix"}],
        **fields,
    }
    config = tmp_path / "probe.json"
    config.write_text(json.dumps({"args": args}), encoding="utf-8")
    result = process.run(
        ["node", str(PROBE), str(WORKFLOW), str(config)],
        capture_output=True, text=True, timeout=15,
    )
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


@pytest.mark.parametrize("fields", [
    {"github_repo": "fork/project"},
    {"plan": {"repo": "fork/project"}},
])
def test_classification_receives_explicit_forge_identity(tmp_path: Path, fields: dict) -> None:
    result = probe(tmp_path, **fields)
    assert "error" not in result, result
    assert len(result["prompts"]) == 1, result
    prompt = result["prompts"][0]["prompt"]
    assert "GitHub repository: fork/project" in prompt, prompt


@pytest.mark.parametrize("fields", [
    {},
    {"github_repo": "fork/project", "plan": {"repo": "upstream/project"}},
    {"github_repo": "/absolute/checkout"},
])
def test_missing_invalid_or_conflicting_identity_stops_before_any_agent(
    tmp_path: Path, fields: dict
) -> None:
    result = probe(tmp_path, **fields)
    assert "GitHub repository" in result.get("error", ""), result
    assert result["prompts"] == [], result


def test_watcher_timeout_is_not_an_arbitrary_two_observation_cutoff() -> None:
    text = (ROOT / "crates/clud-bin/assets/skills/grind-land/SKILL.md").read_text(
        encoding="utf-8"
    )
    assert "a second timeout is `gave_up`" not in text
    assert "A second\n     watchdog stop is `gave_up`" not in text
    assert "same CI" in text
