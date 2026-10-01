"""Tests for the bundled transcript analyzer (issue #1276).

The fixture is synthetic but shaped like the incident: two capped responses
(32,000 output tokens each, usage repeated on every row) that emit 294 and
371 copies of one Bash call, three compact_boundary rows and a terminal
context error.
"""

from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "diagnostics" / "transcript_report.py"

SECRET_CMD_A = "cat /private/bench/SECRET_ARTIFACT_A.json | grep sk-or-v1-TOPSECRET"
SECRET_CMD_B = "rg --no-heading PRIVATE_SYMBOL_B /home/user/secret-project"
SECRET_OUTPUT = "RESULT-BODY-SHOULD-NOT-LEAK " * 60
SECRET_PROMPT = "USER-PROMPT-SHOULD-NOT-LEAK investigate mimalloc"
SESSION_ID = "622e60e1-5376-44bd-be2c-059a72786a16"


@pytest.fixture
def tr():
    name = "clud_test_transcript_report"
    spec = importlib.util.spec_from_file_location(name, SCRIPT)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    try:
        yield module
    finally:
        sys.modules.pop(name, None)


def _assistant(mid, ts, blocks, *, out, inp, cache=0, stop=None):
    return {
        "type": "assistant",
        "sessionId": SESSION_ID,
        "timestamp": ts,
        "message": {
            "id": mid,
            "role": "assistant",
            "content": blocks,
            "stop_reason": stop,
            "usage": {
                "input_tokens": inp,
                "cache_read_input_tokens": cache,
                "output_tokens": out,
            },
        },
    }


def _result(ts, text):
    return {
        "type": "user",
        "sessionId": SESSION_ID,
        "timestamp": ts,
        "message": {
            "role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "t", "content": text}],
        },
    }


def _compact(ts, pre):
    return {
        "type": "system",
        "subtype": "compact_boundary",
        "timestamp": ts,
        "content": "Conversation compacted",
        "compactMetadata": {"trigger": "auto", "preTokens": pre},
    }


def _burst(mid, ts, cmd, n):
    rows = []
    for i in range(n):
        block = {"type": "tool_use", "id": f"{mid}-{i}", "name": "Bash", "input": {"command": cmd}}
        rows.append(_assistant(mid, ts, [block], out=32_000, inp=150_000, stop="max_tokens"))
        rows.append(_result(ts, SECRET_OUTPUT))
    return rows


def _text(t):
    return {"type": "text", "text": t}


def incident_rows():
    rows = [
        {
            "type": "user",
            "timestamp": "2026-09-22T08:30:00Z",
            "message": {"role": "user", "content": SECRET_PROMPT},
        },
        _assistant("msg_small", "2026-09-22T08:30:05Z", [_text("ok")], out=40, inp=20_000),
        _compact("2026-09-22T08:59:24Z", 180_000),
        _assistant("msg_post1", "2026-09-22T09:00:00Z", [_text("x")], out=10, inp=30_000),
    ]
    rows += _burst("msg_burst_a", "2026-09-22T09:01:13Z", SECRET_CMD_A, 294)
    rows.append(_compact("2026-09-22T09:06:16Z", 190_000))
    rows += _burst("msg_burst_b", "2026-09-22T09:07:20Z", SECRET_CMD_B, 371)
    rows.append(_compact("2026-09-22T09:12:59Z", 195_000))
    rows.append(
        {
            "type": "system",
            "timestamp": "2026-09-22T09:13:22Z",
            "content": (
                "Autocompact is thrashing: the context refilled to the previous"
                " compact, 3 times in a row."
            ),
        }
    )
    rows.append(
        {
            "type": "system",
            "timestamp": "2026-09-22T09:13:22Z",
            "content": "Goal cleared after an unrecoverable error (context limit reached)",
        }
    )
    return rows


def test_incident_bursts_reported_with_tokens_counted_once(tr):
    report = tr.analyze(incident_rows())
    bursts = sorted(report["repeated_call_bursts"], key=lambda b: b["copies"])
    assert [b["copies"] for b in bursts] == [294, 371]
    assert all(b["tool"] == "Bash" for b in bursts)
    assert all(b["longest_streak"] == b["copies"] for b in bursts)
    assert all(b["output_tokens"] == 32_000 for b in bursts)
    assert report["responses"] == 4
    # 40 + 10 + 32,000 + 32,000: never 32,000 x 665.
    assert report["output_tokens_total"] == 64_050
    assert report["max_output_tokens_single_response"] == 32_000
    assert report["tool_calls_total"] == 665


def test_incident_compactions_errors_and_results(tr):
    report = tr.analyze(incident_rows())
    assert [c["timestamp"] for c in report["compactions"]] == [
        "2026-09-22T08:59:24Z",
        "2026-09-22T09:06:16Z",
        "2026-09-22T09:12:59Z",
    ]
    kinds = [e["kind"] for e in report["context_errors"]]
    assert kinds == ["autocompact_thrashing", "context_limit_reached"]
    assert report["tool_results"]["count"] == 665
    assert report["context_jumps"], "the 30k -> 150k refill must show as a jump"
    assert report["max_context_tokens"]["value"] is None
    assert "unavailable" in report["max_context_tokens"]["source"]


def test_small_normal_session_has_no_bursts(tr):
    rows = [
        _assistant(
            "m1",
            "t",
            [
                {"type": "tool_use", "name": "Bash", "input": {"command": "ls"}},
                {"type": "tool_use", "name": "Bash", "input": {"command": "ls"}},
                {"type": "tool_use", "name": "Read", "input": {"file_path": "a"}},
            ],
            out=100,
            inp=1000,
        )
    ]
    report = tr.analyze(rows)
    assert report["repeated_call_bursts"] == []
    assert report["tool_calls_total"] == 3


def test_report_never_leaks_raw_content(tr, tmp_path, capsys):
    path = tmp_path / "session.jsonl"
    body = "\n".join(json.dumps(r) for r in incident_rows()) + "\nnot json\n"
    path.write_text(body, encoding="utf-8")
    before = sorted(p.name for p in tmp_path.iterdir())
    for argv in ([str(path)], [str(path), "--json"]):
        assert tr.main(argv) == 0
        out, err = capsys.readouterr()
        blob = out + err
        for secret in (
            "SECRET_ARTIFACT_A",
            "sk-or-v1",
            "PRIVATE_SYMBOL_B",
            "secret-project",
            "RESULT-BODY",
            "USER-PROMPT",
            SESSION_ID,
            "msg_burst_a",
            "thrashing: the context",
        ):
            assert secret not in blob, f"{secret!r} leaked into the report"
        assert "371" in blob
        assert "294" in blob
    assert sorted(p.name for p in tmp_path.iterdir()) == before, "the tool must write no files"


def test_fingerprints_are_salted_per_invocation(tr):
    a = tr.analyze(incident_rows(), salt=b"a" * 16)
    b = tr.analyze(incident_rows(), salt=b"b" * 16)
    fa = {x["call_fingerprint"] for x in a["repeated_call_bursts"]}
    fb = {x["call_fingerprint"] for x in b["repeated_call_bursts"]}
    assert len(fa) == 2
    assert fa.isdisjoint(fb)


def test_missing_file_exits_one(tr, tmp_path, capsys):
    assert tr.main([str(tmp_path / "nope.jsonl")]) == 1
    assert "cannot read transcript" in capsys.readouterr().err
