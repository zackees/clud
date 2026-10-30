"""End-to-end coverage for the Claude -> Gemini request translator.

Ports CLIProxyAPI
``internal/translator/gemini/claude/gemini_claude_request.go``
(``ConvertClaudeRequestToGemini``). The Rust unit tests cover the mapping table;
these drive the real binary through its hidden ``__claude-gemini-translate``
seam, so the process boundary (stdin in, ``generateContent`` body out, exit
status on refusal) is exercised too.
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
        [str(_clud_binary()), "__claude-gemini-translate", *extra],
        input=json.dumps(request),
        capture_output=True,
        text=True,
        timeout=30,
    )


def test_messages_request_becomes_a_generate_content_request() -> None:
    completed = _translate(
        {
            "model": "claude-sonnet-4",
            "max_tokens": 2048,
            "system": [{"type": "text", "text": "be brief"}],
            "messages": [
                {"role": "user", "content": "hi"},
                {
                    "role": "assistant",
                    "content": (
                        [{"type": "text", "text": "hello"}]
                    ),
                },
            ],
        },
        "--model",
        "gemini-2.5-pro",
    )
    assert completed.returncode == 0, completed.stderr
    body = json.loads(completed.stdout)
    assert body["model"] == "gemini-2.5-pro"
    assert body["systemInstruction"]["parts"][0]["text"] == "be brief"
    assert [turn["role"] for turn in body["contents"]] == ["user", "model"]
    assert body["generationConfig"]["maxOutputTokens"] == 2048
    assert len(body["safetySettings"]) == 5


def test_a_tool_call_round_trips_through_the_translator() -> None:
    completed = _translate(
        {
            "model": "claude-sonnet-4",
            "messages": [
                {"role": "user", "content": "read a.rs"},
                {
                    "role": "assistant",
                    "content": [
                        {
                            "type": "tool_use",
                            "id": "toolu_1",
                            "name": "Read.File",
                            "input": {"path": "a.rs"},
                        }
                    ],
                },
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "toolu_1",
                            "content": "fn main() {}",
                        }
                    ],
                },
            ],
            "tools": [
                {
                    "name": "Read.File",
                    "description": "read",
                    "input_schema": {"type": "object"},
                }
            ],
            "tool_choice": {"type": "any"},
        }
    )
    assert completed.returncode == 0, completed.stderr
    body = json.loads(completed.stdout)
    call = body["contents"][1]["parts"][0]["functionCall"]
    assert call["name"] == "Read_File"
    assert call["args"] == {"path": "a.rs"}
    response = body["contents"][2]["parts"][0]["functionResponse"]
    assert response["name"] == "Read_File"
    assert response["response"]["result"] == "fn main() {}"
    declaration = body["tools"][0]["functionDeclarations"][0]
    assert declaration["name"] == "Read_File"
    assert declaration["parametersJsonSchema"] == {"type": "object"}
    assert body["toolConfig"]["functionCallingConfig"]["mode"] == "ANY"


def test_an_untranslatable_part_is_a_nonzero_exit_with_a_named_reason() -> None:
    completed = _translate(
        {
            "model": "claude-sonnet-4",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "image",
                            "source": {"type": "url", "url": "https://example.test/a.png"},
                        }
                    ],
                }
            ],
        }
    )
    assert completed.returncode == 2
    assert "url" in completed.stderr
    assert "Gemini part" in completed.stderr


def test_a_request_without_a_model_is_refused() -> None:
    completed = _translate({"messages": [{"role": "user", "content": "hi"}]})
    assert completed.returncode == 2
    assert "names no model" in completed.stderr
