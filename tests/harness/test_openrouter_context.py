"""#2225: real clud + real Claude Code, with all model replies scripted locally."""

from __future__ import annotations

import json
import shlex
import sys
from pathlib import Path

import pytest

from tests.harness.context_adversaries import adversarial_cases
from tests.harness.context_audit import audit_context, audit_retry
from tests.harness.harness import Harness, RunResult

TURNS = 8
OUTPUT_BYTES = 4096
INSTRUCTION = "CONTEXT_AUDIT_PROJECT_INSTRUCTION_ONCE"
UNRELATED = "CONTEXT_AUDIT_UNRELATED_FILE_MUST_NOT_LEAK"
MODEL = "deepseek/deepseek-v4.1-flash-1017"
INITIAL_PROMPT = "Run the scripted read-only context audit and stop at its completion marker."
RESUME_PROMPT = "Continue the read-only audit and return the resume completion marker."


def _prepare(h: Harness) -> tuple[dict, list[str], list[str]]:
    root = Path(__file__).resolve().parents[2]
    (h.repo / "CLAUDE.md").write_text(
        (root / "CLAUDE.md").read_text(encoding="utf-8") + f"\n{INSTRUCTION}\n", encoding="utf-8"
    )
    (h.root / "unrelated-private-fixture.txt").write_text(UNRELATED, encoding="utf-8")
    emit = h.root / "emit.py"
    markers = [f"CONTEXT_AUDIT_RESULT_{i}:" for i in range(TURNS)]
    escaped = 'Quotes " and backslash \\; tab\t; newline\nUnicode 雪🙂\n'
    payloads = [
        f"{marker} " + escaped + chr(65 + index) * (OUTPUT_BYTES - len(escaped.encode("utf-8")))
        for index, marker in enumerate(markers)
    ]
    emit.write_text(f"import sys\nprint({payloads!r}[int(sys.argv[1])])\n", encoding="utf-8")
    steps = []
    for index in range(TURNS):
        step = {
            "tool_use": {
                "name": "Bash",
                "input": {
                    "command": f"{shlex.quote(sys.executable)} {shlex.quote(str(emit))} {index}",
                    "description": "Read one fixed-size synthetic output",
                },
            }
        }
        if index:
            step["expect"] = {"is_error": False, "content_contains": markers[index - 1]}
        steps.append(step)
    steps.append(
        {
            "expect": {"is_error": False, "content_contains": markers[-1]},
            "text": "CONTEXT_AUDIT_COMPLETE",
        }
    )
    return (
        {
            "audit_http": True,
            "forbidden_context_markers": [UNRELATED],
            "default_text": "UNEXPECTED_EXTRA_REQUEST",
            "roles": [{"name": "main", "steps": steps}],
        },
        markers,
        payloads,
    )


def _check(
    requests: list[dict], markers: list[str], payloads: list[str], calls: list[dict]
) -> dict:
    return audit_context(
        requests,
        instruction_marker=INSTRUCTION,
        output_markers=markers,
        initial_budget=120_000,
        growth_budget=OUTPUT_BYTES + 2048,
        forbidden_markers=(UNRELATED,),
        expected_payloads=payloads,
        expected_tool_calls=calls,
        instruction_sections=[
            (Path(__file__).resolve().parents[2] / "CLAUDE.md").read_text(encoding="utf-8"),
            INITIAL_PROMPT,
        ],
    )


def _audit_run(
    result: RunResult,
    profile: str,
    markers: list[str],
    payloads: list[str],
    expected_calls: list[dict],
) -> dict:
    assert result.returncode == 0, result.stdout[-3000:] + result.stderr[-3000:]
    assert "CONTEXT_AUDIT_COMPLETE" in result.stdout
    calls = [row for row in result.requests if row.get("role")]
    http = [row for row in result.requests if row.get("kind") == "http_audit"]
    assert all(
        row["path"] in {"/v1/messages", "/v1/key", "/v1/models", "/v1/messages/count_tokens"}
        for row in http
    ), http
    assert sum(row["path"] == "/v1/messages" for row in http) == len(calls)
    assert all(row["model"] == MODEL and not row.get("note") for row in calls)
    assert all(row["context_bytes"]["tools"] > 0 for row in calls)
    successful = calls
    if profile == "retry":
        assert [row["response_status"] for row in calls] == [503] + [200] * (TURNS + 1)
        audit_retry(calls[0], calls[1])
        import copy

        corrupted = copy.deepcopy(calls[0])
        corrupted["context_bytes"]["body_fingerprint"] = "same-length-body-change"
        with pytest.raises(AssertionError):
            audit_retry(corrupted, calls[1])
        successful = calls[1:]
    else:
        assert [row["response_status"] for row in calls] == [200] * (TURNS + 1)
    report = _check(successful, markers, payloads, expected_calls)
    report["attempt_count"] = len(calls)
    if profile == "retry":
        report["same_size_retry_mutation_rejected"] = True
    report["wire_bytes_including_retries"] = sum(row["context_bytes"]["request"] for row in calls)
    report["http_paths"] = sorted({row["path"] for row in http})
    rejected = []
    for name, mutated in adversarial_cases(successful, INSTRUCTION, UNRELATED):
        try:
            _check(mutated, markers, payloads, expected_calls)
        except AssertionError:
            rejected.append(name)
        else:
            pytest.fail(f"context audit accepted adversary: {name}")
    report["rejected_adversaries"] = rejected
    return report


def _audit_resume(
    h: Harness,
    session: str,
    previous: list[dict],
    markers: list[str],
    payloads: list[str],
    expected_calls: list[dict],
) -> dict:
    h.new_session()
    result = h.run_clud(
        RESUME_PROMPT,
        {
            "audit_http": True,
            "forbidden_context_markers": [UNRELATED],
            "default_text": "CONTEXT_AUDIT_RESUME_COMPLETE",
        },
        model=MODEL,
        extra_args=["--resume", session, "--fork-session"],
        timeout=60,
    )
    assert result.returncode == 0, result.stdout[-3000:] + result.stderr[-3000:]
    assert "CONTEXT_AUDIT_RESUME_COMPLETE" in result.stdout
    calls = [row for row in result.requests if row.get("role")]
    assert len(calls) == 1, "resume replayed model calls instead of loading history"
    assert calls[0]["model"] == MODEL
    assert calls[0]["response_status"] == 200
    assert not calls[0].get("note")
    http = [row for row in result.requests if row.get("kind") == "http_audit"]
    assert sum(row["path"] == "/v1/messages" for row in http) == 1
    assert all(
        row["path"] in {"/v1/messages", "/v1/key", "/v1/models", "/v1/messages/count_tokens"}
        for row in http
    )
    report = audit_context(
        previous + calls,
        instruction_marker=INSTRUCTION,
        output_markers=markers,
        initial_budget=120_000,
        growth_budget=OUTPUT_BYTES + 2048,
        forbidden_markers=(UNRELATED,),
        history_counts=[*range(TURNS + 1), TURNS],
        extra_context_texts={TURNS + 1: [("user", RESUME_PROMPT)]},
        extra_assistant_texts={TURNS + 1: ["CONTEXT_AUDIT_COMPLETE"]},
        expected_tool_calls=expected_calls,
        instruction_sections=[
            (Path(__file__).resolve().parents[2] / "CLAUDE.md").read_text(encoding="utf-8"),
            INITIAL_PROMPT,
        ],
        expected_payloads=payloads,
    )
    report["captured_resume_attempts"] = len(calls)
    return report


def test_openrouter_context_stays_bounded_across_real_tool_turns(harness: Harness) -> None:
    h = harness
    script, markers, payloads = _prepare(h)
    expected_calls = [step["tool_use"] for step in script["roles"][0]["steps"][:-1]]
    reports = {}
    default_session = h.session_id
    default_calls = []
    profiles = [
        ("default", []),
        ("restricted", ["--tools", "Bash,Read", "--strict-mcp-config"]),
        ("retry", []),
    ]
    for profile, extra in profiles:
        if profile != "default":
            h.new_session()
        scenario = {**script, "first_response_status": 503} if profile == "retry" else script
        result = h.run_clud(
            INITIAL_PROMPT,
            scenario,
            model=MODEL,
            extra_args=extra,
            timeout=120,
        )
        reports[profile] = _audit_run(result, profile, markers, payloads, expected_calls)
        if profile == "default":
            default_calls = [row for row in result.requests if row.get("role")]
    assert (
        reports["restricted"]["initial_request_bytes"] < reports["default"]["initial_request_bytes"]
    )
    reports["resume"] = _audit_resume(
        h, default_session, default_calls, markers, payloads, expected_calls
    )
    (h.logs / "context-audit.json").write_text(json.dumps(reports, indent=2), encoding="utf-8")
    print(json.dumps({"context_audit": reports}))
