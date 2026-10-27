"""End-to-end coverage for the OpenAI Chat Completions ingress translator.

Ports CLIProxyAPI
``internal/translator/claude/openai/chat-completions/claude_openai_request.go``.
The Rust unit tests cover the mapping table; these run the real binary through
its hidden ``__openai-chat-translate`` seam, so the process boundary (stdin in,
Messages body out, exit status on refusal) is exercised too.
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
    candidate = Path(__file__).resolve().parents[1] / "target" / "debug" / _binary_name("clud")
    assert candidate.is_file(), f"clud test binary not found at {candidate}"
    return candidate


def _translate(request: dict[str, object]) -> process.CompletedProcess[object]:
    return process.run(
        [str(_clud_binary()), "__openai-chat-translate"],
        input=json.dumps(request),
        capture_output=True,
        text=True,
        timeout=30,
    )


def test_openai_chat_request_becomes_a_messages_request() -> None:
    completed = _translate(
        {
            "model": "claude-sonnet",
            "messages": [
                {"role": "system", "content": "be terse"},
                {"role": "user", "content": "hello"},
                {
                    "role": "assistant",
                    "content": None,
                    "tool_calls": [
                        {
                            "id": "call_1",
                            "type": "function",
                            "function": {"name": "read_file", "arguments": '{"path":"a.rs"}'},
                        }
                    ],
                },
                {"role": "tool", "tool_call_id": "call_1", "content": "fn main() {}"},
            ],
            "max_completion_tokens": 512,
            "tool_choice": "required",
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "description": "read a file",
                        "parameters": {
                            "type": "object",
                            "properties": {"path": {"type": "string"}},
                        },
                    },
                }
            ],
        }
    )

    assert completed.returncode == 0, completed.stderr
    translated = json.loads(completed.stdout)

    assert translated["model"] == "claude-sonnet"
    assert translated["max_tokens"] == 512
    assert translated["stream"] is False
    assert translated["system"][0]["text"] == "be terse"
    assert translated["messages"][0]["role"] == "user"
    assert translated["messages"][0]["content"][0]["text"] == "hello"
    assert translated["messages"][1]["content"][0]["type"] == "tool_use"
    assert translated["messages"][1]["content"][0]["input"] == {"path": "a.rs"}
    assert translated["messages"][2]["content"][0]["type"] == "tool_result"
    assert translated["messages"][2]["content"][0]["tool_use_id"] == "call_1"
    assert translated["tools"][0]["name"] == "read_file"
    assert translated["tool_choice"]["type"] == "any"


def test_unsupported_content_part_is_refused_with_its_name() -> None:
    completed = _translate(
        {
            "model": "claude-sonnet",
            "messages": [
                {
                    "role": "user",
                    "content": [{"type": "input_audio", "input_audio": {"data": "AAAA"}}],
                }
            ],
        }
    )

    assert completed.returncode == 2
    assert "input_audio" in completed.stderr


def test_missing_model_is_refused() -> None:
    completed = _translate({"messages": [{"role": "user", "content": "hi"}]})

    assert completed.returncode == 2
    assert "model" in completed.stderr
