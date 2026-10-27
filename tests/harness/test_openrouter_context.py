"""#2225: real clud + real Claude Code, with all model replies scripted locally."""

from __future__ import annotations

import json
import shlex
import sys
from pathlib import Path

from tests.harness.context_audit import audit_context
from tests.harness.harness import Harness

TURNS = 8
OUTPUT_BYTES = 4096
INSTRUCTION = "CONTEXT_AUDIT_PROJECT_INSTRUCTION_ONCE"


def _prepare(h: Harness) -> tuple[dict, list[str]]:
    root = Path(__file__).resolve().parents[2]
    (h.repo / "CLAUDE.md").write_text(
        (root / "CLAUDE.md").read_text(encoding="utf-8") + f"\n{INSTRUCTION}\n", encoding="utf-8"
    )
    emit = h.root / "emit.py"
    emit.write_text(
        "import sys\n" + f"print('CONTEXT_AUDIT_RESULT_' + sys.argv[1] + ': ' + 'x' * {OUTPUT_BYTES})\n",
        encoding="utf-8",
    )
    markers = [f"CONTEXT_AUDIT_RESULT_{i}:" for i in range(TURNS)]
    steps = []
    for index in range(TURNS):
        step = {"tool_use": {"name": "Bash", "input": {
            "command": f"{shlex.quote(sys.executable)} {shlex.quote(str(emit))} {index}",
            "description": "Read one fixed-size synthetic output",
        }}}
        if index:
            step["expect"] = {"is_error": False, "content_contains": markers[index - 1]}
        steps.append(step)
    steps.append({"expect": {"is_error": False, "content_contains": markers[-1]},
                  "text": "CONTEXT_AUDIT_COMPLETE"})
    return {"default_text": "UNEXPECTED_EXTRA_REQUEST", "roles": [{"name": "main", "steps": steps}]}, markers


def test_openrouter_context_stays_bounded_across_real_tool_turns(harness: Harness) -> None:
    h = harness
    script, markers = _prepare(h)
    reports = {}
    for profile, extra in [("default", []), ("restricted", ["--tools", "Bash,Read",
                                                          "--strict-mcp-config"])]:
        if profile != "default":
            h.new_session()
        result = h.run_clud(
            "Run the scripted read-only context audit and stop at its completion marker.",
            script, model="deepseek/deepseek-v4.1-flash-1017", extra_args=extra, timeout=120,
        )
        assert result.returncode == 0, result.stdout[-3000:] + result.stderr[-3000:]
        assert "CONTEXT_AUDIT_COMPLETE" in result.stdout
        assert not any(request.get("note") for request in result.requests)
        reports[profile] = audit_context(
            result.requests, instruction_marker=INSTRUCTION, output_markers=markers,
            initial_budget=120_000, growth_budget=OUTPUT_BYTES + 8000,
        )
        assert all(request["context_bytes"]["tools"] > 0 for request in result.requests)
    assert reports["restricted"]["initial_request_bytes"] < reports["default"]["initial_request_bytes"]
    (h.logs / "context-audit.json").write_text(json.dumps(reports, indent=2), encoding="utf-8")
    print(json.dumps({"context_audit": reports}))
