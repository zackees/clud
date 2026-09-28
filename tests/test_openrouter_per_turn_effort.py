"""Opt-in, secret-free fixture for the #1528 per-turn-effort injection.

Run with ``CLUD_REAL_CLAUDE_TESTS=1 uv run pytest -q
tests/test_openrouter_per_turn_effort.py``. The fixture drives the installed
Claude Code binary headless against a loopback-only Anthropic-format stub,
supplies a fixed dummy credential, and reads the request bodies back out of the
stub's log. Nothing leaves 127.0.0.1 and nothing is persisted outside the
test's temporary directory.

What is under test: Claude Code preserves the prompt cache across a mid-session
effort change only when its client-side ``per_turn_effort`` capability is on
for the launched wire ID. clud supplies that through
``CLAUDE_CODE_MODEL_CAPABILITIES``. RED is the run without that variable (the
turn-2 request carries no appended ``output_config`` system message, and the
``per-turn-control-<date>`` beta is absent); GREEN is the same run with
``<wire>=per_turn_effort`` set (the message appears and the beta with it).

Modelled on ``tests/test_real_claude_unified_effort.py``, which established the
pattern: an in-process ``ThreadingHTTPServer``, a throwaway ``HOME``, and a
unique ``--session-id`` per case.
"""

from __future__ import annotations

import json
import os
import shutil
import threading
import uuid
from collections.abc import Iterator
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, ClassVar

import pytest

from tests import process

WIRE_MODEL = "xiaomi/mimo-v2.6-flash"
"""The issue's live row: OpenRouter does not tag it `reasoning_effort`, so the
harness's own catalog never grants `per_turn_effort` and only the child-env
override can flip it. That is exactly the RED -> GREEN pair."""

CAPABILITY_ENV = "CLAUDE_CODE_MODEL_CAPABILITIES"
PER_TURN_CONTROL_BETA = "per-turn-control-2026-07-01"
FIRST_EFFORT, SECOND_EFFORT = "low", "high"

SkipUnlessRealClaude = pytest.mark.skipif(
    os.environ.get("CLUD_REAL_CLAUDE_TESTS") != "1",
    reason="set CLUD_REAL_CLAUDE_TESTS=1 to run the installed Claude Code fixture",
)


def _sse(*events: tuple[str, dict[str, Any]]) -> bytes:
    return "".join(
        f"event: {name}\ndata: {json.dumps(payload)}\n\n" for name, payload in events
    ).encode()


def _completion_body(model: str) -> bytes:
    """The canned Anthropic streaming sequence the stub answers with."""
    return _sse(
        (
            "message_start",
            {
                "type": "message_start",
                "message": {
                    "id": "msg_fixture",
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "content": [],
                    "stop_reason": None,
                    "stop_sequence": None,
                    "usage": {"input_tokens": 7, "output_tokens": 1},
                },
            },
        ),
        (
            "content_block_start",
            {
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text", "text": ""},
            },
        ),
        (
            "content_block_delta",
            {
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": "fixture"},
            },
        ),
        ("content_block_stop", {"type": "content_block_stop", "index": 0}),
        (
            "message_delta",
            {
                "type": "message_delta",
                "delta": {"stop_reason": "end_turn", "stop_sequence": None},
                "usage": {"output_tokens": 1},
            },
        ),
        ("message_stop", {"type": "message_stop"}),
    )


def _handler_class(log_path: Path) -> type[BaseHTTPRequestHandler]:
    """One stub instance per turn, each with its own JSON-lines request log."""

    class _StubHandler(BaseHTTPRequestHandler):
        requests: ClassVar[list[dict[str, Any]]] = []

        def _record(self, body: Any) -> None:
            entry = {
                "method": self.command,
                "path": self.path,
                "headers": {name.lower(): value for name, value in self.headers.items()},
                "body": body,
            }
            type(self).requests.append(entry)
            with log_path.open("a", encoding="utf-8") as handle:
                handle.write(json.dumps(entry) + "\n")

        def _json(self, status: int, payload: Any) -> None:
            body = json.dumps(payload).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self) -> None:
            self._record(None)
            if self.path.split("?", 1)[0] == "/v1/models":
                self._json(
                    200,
                    {
                        "data": [
                            {"id": WIRE_MODEL, "display_name": "Fixture", "type": "model"}
                        ],
                        "has_more": False,
                    },
                )
            else:
                self._json(404, {"type": "error", "error": {"type": "not_found_error"}})

        def do_HEAD(self) -> None:
            self._record(None)
            self.send_response(200)
            self.send_header("Content-Length", "0")
            self.end_headers()

        def do_POST(self) -> None:
            length = int(self.headers.get("content-length", "0"))
            raw = self.rfile.read(length)
            try:
                body = json.loads(raw)
            except json.JSONDecodeError:
                body = {"_invalid_json": True}
            self._record(body)
            path = self.path.split("?", 1)[0]
            if path == "/v1/messages/count_tokens":
                self._json(200, {"input_tokens": 7})
                return
            if path != "/v1/messages":
                self._json(404, {"type": "error", "error": {"type": "not_found_error"}})
                return
            model = body.get("model") if isinstance(body, dict) else None
            response = _completion_body(model if isinstance(model, str) else WIRE_MODEL)
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("Content-Length", str(len(response)))
            self.end_headers()
            self.wfile.write(response)

        def log_message(self, _format: str, *_args: object) -> None:
            return

    return _StubHandler


@contextmanager
def _stub_gateway(directory: Path) -> Iterator[tuple[str, Path, list[dict[str, Any]]]]:
    """A loopback Anthropic-format stub with a JSON-lines log file."""
    directory.mkdir(parents=True, exist_ok=True)
    log_path = directory / "requests.jsonl"
    handler = _handler_class(log_path)
    handler.requests = []
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}", log_path, handler.requests
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


def _claude_command() -> str | None:
    override = os.environ.get("CLUD_REAL_CLAUDE")
    if override:
        return override
    resolved = shutil.which("claude")
    if resolved is None or os.name != "nt":
        return resolved
    # npm exposes a .cmd wrapper, while running-process deliberately launches
    # executables without an implicit shell. Prefer Claude Code's adjacent
    # native binary when the standard npm layout is present.
    native = (
        Path(resolved).parent
        / "node_modules"
        / "@anthropic-ai"
        / "claude-code"
        / "bin"
        / "claude.exe"
    )
    return str(native) if native.is_file() else resolved


def _isolated_claude_env(home: Path, base_url: str, capabilities: str | None) -> dict[str, str]:
    """The minimum OS environment, plus a dummy credential and a throwaway HOME."""
    allowed = (
        "COMSPEC",
        "LANG",
        "LC_ALL",
        "PATH",
        "PATHEXT",
        "SYSTEMROOT",
        "TEMP",
        "TERM",
        "TMP",
        "WINDIR",
    )
    env = {name: os.environ[name] for name in allowed if name in os.environ}
    config = home / "claude-config"
    app_data = home / "app-data"
    local_app_data = home / "local-app-data"
    for directory in (home, config, app_data, local_app_data):
        directory.mkdir(parents=True, exist_ok=True)
    env.update(
        {
            "HOME": str(home),
            "USERPROFILE": str(home),
            "APPDATA": str(app_data),
            "LOCALAPPDATA": str(local_app_data),
            "CLAUDE_CONFIG_DIR": str(config),
            "ANTHROPIC_BASE_URL": base_url,
            "ANTHROPIC_API_KEY": "sk-dummy",
            "TERM": "dumb",
            "NO_PROXY": "127.0.0.1,localhost",
            # clud's own subprocess harnesses never read a developer's server
            # settings cache; keep that true here too.
            "CLUD_SERVER_SETTINGS": "0",
        }
    )
    if capabilities is not None:
        env[CAPABILITY_ENV] = capabilities
    return env


def _turn(
    claude: str,
    home: Path,
    directory: Path,
    prompt: str,
    effort: str,
    *,
    session_id: str,
    resume: bool,
    capabilities: str | None,
) -> dict[str, Any]:
    """Run one turn against its own stub; return the captured request."""
    with _stub_gateway(directory) as (base_url, log_path, requests):
        env = _isolated_claude_env(home, base_url, capabilities)
        command = [
            claude,
            "--print",
            "--output-format",
            "stream-json",
            "--verbose",
            "--model",
            WIRE_MODEL,
            "--effort",
            effort,
            "--tools",
            "",
            "--setting-sources",
            "",
        ]
        command += ["--resume", session_id] if resume else ["--session-id", session_id]
        command.append(prompt)
        process.run(
            command,
            capture_output=True,
            text=True,
            timeout=60,
            env=env,
            stdin=process.DEVNULL,
        )
        messages = [
            request
            for request in requests
            if request["method"] == "POST"
            and request["path"].split("?", 1)[0] == "/v1/messages"
        ]
        # A reused `--session-id` makes the harness exit with "Session ID ... is
        # already in use" and send nothing at all, which would otherwise read as
        # a passing assertion about an empty request set.
        assert messages, f"Claude Code captured no Messages request for effort={effort}"
        assert log_path.exists(), log_path
        return messages[-1]


def _init_frame(stdout: str) -> dict[str, Any]:
    for line in stdout.splitlines():
        try:
            frame = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (
            isinstance(frame, dict)
            and frame.get("type") == "system"
            and frame.get("subtype") == "init"
        ):
            return frame
    raise AssertionError(f"no system/init frame in stream-json output: {stdout!r}")


def _appended_control(messages: list[Any]) -> dict[str, Any] | None:
    """The trailing per-turn control message, when the harness emitted one."""
    for message in reversed(messages):
        if (
            isinstance(message, dict)
            and message.get("role") == "system"
            and message.get("content") == []
            and isinstance(message.get("output_config"), dict)
        ):
            return message
    return None


def _conversation(messages: list[Any]) -> list[Any]:
    """The conversation turns, with the harness's own environment block pinned.

    The in-conversation environment block is serialized as a content-block
    *array* on the first turn and as a plain *string* on a resumed turn. That
    is #1528's open item 3 -- a separate possible cache-miss source with its own
    investigation -- so comparing it byte for byte would make this fixture fail
    for a reason it does not own. Its `content` is replaced with a sentinel and
    every other field (including `output_config`) is compared verbatim. The
    appended per-turn control carries `content: []` and is never rewritten.
    """
    normalized: list[Any] = []
    for message in messages:
        if (
            isinstance(message, dict)
            and message.get("role") == "system"
            and message.get("content") != []
        ):
            normalized.append({**message, "content": "<environment>"})
        else:
            normalized.append(message)
    return normalized


def _run_case(tmp_path: Path, capabilities: str | None) -> dict[str, Any]:
    claude = _claude_command()
    if claude is None:
        pytest.fail("Claude Code is not installed; set CLUD_REAL_CLAUDE to its executable")

    case = tmp_path / ("green" if capabilities else "red")
    session_id = str(uuid.uuid4())
    first = _turn(
        claude,
        case / "home",
        case / "turn-1",
        "Return the word fixture.",
        FIRST_EFFORT,
        session_id=session_id,
        resume=False,
        capabilities=capabilities,
    )
    second = _turn(
        claude,
        case / "home",
        case / "turn-2",
        "Return the word fixture again.",
        SECOND_EFFORT,
        session_id=session_id,
        resume=True,
        capabilities=capabilities,
    )
    return {"first": first, "second": second}


@SkipUnlessRealClaude
@pytest.mark.real_claude
def test_per_turn_effort_is_injected_only_with_the_capability_env(tmp_path: Path) -> None:
    """RED -> GREEN: the child-env capability is what changes the request bytes."""
    red = _run_case(tmp_path, None)
    green = _run_case(tmp_path, f"{WIRE_MODEL}=per_turn_effort")

    for label, case in (("red", red), ("green", green)):
        first, second = case["first"], case["second"]
        # Both turns really went over the wire against the loopback stub.
        assert first["path"].split("?", 1)[0] == "/v1/messages"
        assert first["body"]["model"] == WIRE_MODEL
        assert second["body"]["model"] == WIRE_MODEL

        # The effort level travels top-level on both turns either way.
        assert first["body"]["output_config"]["effort"] == FIRST_EFFORT
        assert second["body"]["output_config"]["effort"] == SECOND_EFFORT

        # Turn 1's conversation is an exact serialized prefix of turn 2's: the
        # resume replayed history rather than rebuilding it.
        first_conversation = _conversation(first["body"]["messages"])
        second_conversation = _conversation(second["body"]["messages"])
        assert len(second_conversation) > len(first_conversation), label
        assert (
            json.dumps(second_conversation[: len(first_conversation)])
            == json.dumps(first_conversation)
        ), label

        appended = _appended_control(second["body"]["messages"])
        beta = second["headers"].get("anthropic-beta", "")
        expected = label == "green"
        assert (appended is not None) is expected, f"{label}: {second['body']['messages']!r}"
        assert (PER_TURN_CONTROL_BETA in beta) is expected, f"{label}: {beta!r}"
        if expected:
            assert appended == {
                "role": "system",
                "content": [],
                "output_config": {"effort": SECOND_EFFORT},
            }
        else:
            assert _appended_control(first["body"]["messages"]) is None
            assert PER_TURN_CONTROL_BETA not in first["headers"].get("anthropic-beta", "")


@SkipUnlessRealClaude
@pytest.mark.real_claude
def test_system_init_reports_the_per_turn_effort_capability(tmp_path: Path) -> None:
    """The harness's own `system/init` frame names the capability it resolved."""
    claude = _claude_command()
    if claude is None:
        pytest.fail("Claude Code is not installed; set CLUD_REAL_CLAUDE to its executable")

    expected = {
        None: False,
        f"{WIRE_MODEL}=per_turn_effort": True,
    }
    for capabilities, expected_active in expected.items():
        case = tmp_path / (capabilities or "red")
        session_id = str(uuid.uuid4())
        with _stub_gateway(case / "turn-1") as (base_url, _log, _requests):
            completed = process.run(
                [
                    claude,
                    "--print",
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--model",
                    WIRE_MODEL,
                    "--effort",
                    FIRST_EFFORT,
                    "--tools",
                    "",
                    "--setting-sources",
                    "",
                    "--session-id",
                    session_id,
                    "Return the word fixture.",
                ],
                capture_output=True,
                text=True,
                timeout=60,
                env=_isolated_claude_env(case / "home", base_url, capabilities),
                stdin=process.DEVNULL,
            )
        init = _init_frame(completed.stdout)
        assert init.get("per_turn_effort_active") is expected_active, init
