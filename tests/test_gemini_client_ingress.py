"""End-to-end coverage for the Gemini -> Claude request translator.

Ports CLIProxyAPI
``internal/translator/claude/gemini/claude_gemini_request.go``
(``ConvertGeminiRequestToClaude``). The Rust unit tests cover the mapping table;
these drive the real binary through its hidden ``__gemini-client-translate``
seam, so the process boundary (stdin in, Messages body out, exit status on a
refusal) is exercised too.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

from tests import process


def _binary_name(name: str) -> str:
    return f"{name}.exe" if sys.platform == "win32" else name


def _clud_binary() -> Path:
    configured = os.environ.get("CLUD_TEST_BINARY")
    if configured:
        return Path(configured)
    candidate = (
        Path(__file__).resolve().parents[1] / "target" / "debug" / _binary_name("clud")
    )
    assert candidate.is_file(), f"clud test binary not found at {candidate}"
    return candidate


def _translate(
    request: dict[str, object], *extra: str
) -> process.CompletedProcess[object]:
    return process.run(
        [str(_clud_binary()), "__gemini-client-translate", *extra],
        input=json.dumps(request),
        capture_output=True,
        text=True,
        timeout=30,
    )


def test_generate_content_request_becomes_a_messages_request() -> None:
    completed = _translate(
        {
            "systemInstruction": {"parts": [{"text": "be brief"}]},
            "contents": [
                {"role": "user", "parts": [{"text": "hi"}]},
                {"role": "model", "parts": [{"text": "hello"}]},
            ],
            "generationConfig": {"maxOutputTokens": 2048, "topP": 0.9},
        },
        "--model",
        "claude-sonnet-4",
    )
    assert completed.returncode == 0, completed.stderr
    body = json.loads(completed.stdout)
    assert body["model"] == "claude-sonnet-4"
    assert body["max_tokens"] == 2048
    assert body["top_p"] == 0.9
    assert body["system"] == [{"type": "text", "text": "be brief"}]
    assert [message["role"] for message in body["messages"]] == ["user", "assistant"]
    assert body["messages"][1]["content"] == [{"type": "text", "text": "hello"}]
    assert body["stream"] is False


def test_function_call_and_response_pair_up_across_the_process_seam() -> None:
    completed = _translate(
        {
            "contents": [
                {"role": "user", "parts": [{"text": "read it"}]},
                {
                    "role": "model",
                    "parts": [
                        {"functionCall": {"name": "Read.File", "args": {"path": "a"}}}
                    ],
                },
                {
                    "role": "user",
                    "parts": [
                        {
                            "functionResponse": {
                                "name": "Read.File",
                                "response": {"result": "contents"},
                            }
                        }
                    ],
                },
            ],
            "tools": [
                {
                    "functionDeclarations": [
                        {
                            "name": "Read.File",
                            "parameters": {
                                "type": "OBJECT",
                                "properties": {"path": {"type": "STRING"}},
                            },
                        }
                    ]
                }
            ],
        },
        "--model",
        "claude-sonnet-4",
    )
    assert completed.returncode == 0, completed.stderr
    body = json.loads(completed.stdout)
    tool_use = body["messages"][1]["content"][0]
    assert tool_use["type"] == "tool_use"
    assert tool_use["name"] == "Read_File"
    assert tool_use["input"] == {"path": "a"}
    tool_result = body["messages"][2]["content"][0]
    assert tool_result["tool_use_id"] == tool_use["id"]
    assert tool_result["content"] == "contents"
    assert body["tools"][0]["input_schema"]["properties"]["path"]["type"] == "string"


def test_stream_flag_and_tool_config_reach_the_body() -> None:
    completed = _translate(
        {
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
            "toolConfig": {
                "functionCallingConfig": {
                    "mode": "ANY",
                    "allowedFunctionNames": ["Read.File"],
                }
            },
        },
        "--model",
        "claude-sonnet-4",
        "--stream",
    )
    assert completed.returncode == 0, completed.stderr
    body = json.loads(completed.stdout)
    assert body["stream"] is True
    assert body["tool_choice"] == {"type": "tool", "name": "Read_File"}


def test_the_request_model_is_used_when_no_override_is_given() -> None:
    completed = _translate(
        {
            "model": "clud-claude-openrouter-sonnet",
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
        }
    )
    assert completed.returncode == 0, completed.stderr
    assert json.loads(completed.stdout)["model"] == "clud-claude-openrouter-sonnet"


def test_an_untranslatable_user_attachment_is_refused_by_name() -> None:
    completed = _translate(
        {
            "contents": [
                {
                    "role": "user",
                    "parts": [
                        {"inlineData": {"mimeType": "video/mp4", "data": "AAAA"}}
                    ],
                }
            ]
        },
        "--model",
        "claude-sonnet-4",
    )
    assert completed.returncode == 2
    assert "video/mp4" in completed.stderr
    assert completed.stdout == ""


def test_a_request_with_no_model_and_no_override_is_refused() -> None:
    completed = _translate(
        {"contents": [{"role": "user", "parts": [{"text": "hi"}]}]}
    )
    assert completed.returncode == 2
    assert "names no model" in completed.stderr
