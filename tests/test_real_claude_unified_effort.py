"""Opt-in, secret-free Claude Code protocol fixture for unified effort routing.

Run with ``CLUD_REAL_CLAUDE_TESTS=1 uv run pytest -q
tests/test_real_claude_unified_effort.py``. The fixture points the installed
Claude Code binary at a loopback-only fake, supplies fixed canary credentials,
and records request JSON plus outbound headers in memory. It never persists
them or contacts a model provider.
"""

from __future__ import annotations

import json
import os
import shutil
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, ClassVar

import pytest

from tests import process

DISCOVERY_ID = "clud-claude-codex-terra"
EFFORTS = ("low", "high", "xhigh", "max")


class _GatewayHandler(BaseHTTPRequestHandler):
    requests: ClassVar[list[dict[str, Any]]] = []
    gets: ClassVar[list[str]] = []

    def do_GET(self) -> None:
        type(self).gets.append(self.path)
        body = json.dumps(
            {
                "data": [
                    {
                        "id": DISCOVERY_ID,
                        "display_name": "Codex Terra (OpenAI)",
                        "type": "model",
                    }
                ],
                "has_more": False,
            }
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self) -> None:
        length = int(self.headers.get("content-length", "0"))
        raw = self.rfile.read(length)
        try:
            body = json.loads(raw)
        except json.JSONDecodeError:
            body = {"_invalid_json": True}
        headers = {name.lower(): value for name, value in self.headers.items()}
        type(self).requests.append(
            {"path": self.path, "body": body, "headers": headers}
        )
        # The protocol signal under test is the outbound request. A permanent
        # local response keeps the fixture fast and prevents client retries.
        response = json.dumps(
            {
                "type": "error",
                "error": {
                    "type": "invalid_request_error",
                    "message": "fixture captured request",
                },
            }
        ).encode()
        self.send_response(400)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(response)))
        self.end_headers()
        self.wfile.write(response)

    def log_message(self, _format: str, *_args: object) -> None:
        return


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


def _isolated_claude_env(home: Path, base_url: str) -> dict[str, str]:
    """Build the minimum OS environment around fixture-owned credentials."""
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
            "ANTHROPIC_API_KEY": "fixture-api-key-canary",
            "ANTHROPIC_CUSTOM_HEADERS": (
                "X-Clud-Gateway-Token: fixture-token-canary"
            ),
            "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY": "1",
            "NO_PROXY": "127.0.0.1,localhost",
        }
    )
    return env


@pytest.mark.real_claude
@pytest.mark.skipif(
    os.environ.get("CLUD_REAL_CLAUDE_TESTS") != "1",
    reason="set CLUD_REAL_CLAUDE_TESTS=1 to run the installed Claude Code fixture",
)
def test_synthetic_model_receives_each_cli_effort_at_the_loopback_gateway(
    tmp_path: Path,
) -> None:
    claude = _claude_command()
    if claude is None:
        pytest.fail("Claude Code is not installed; set CLUD_REAL_CLAUDE to its executable")

    _GatewayHandler.requests = []
    _GatewayHandler.gets = []
    server = ThreadingHTTPServer(("127.0.0.1", 0), _GatewayHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    base_url = f"http://127.0.0.1:{server.server_port}"
    try:
        for effort in EFFORTS:
            before = len(_GatewayHandler.requests)
            env = _isolated_claude_env(tmp_path / effort, base_url)
            process.run(
                [
                    claude,
                    "--print",
                    "--model",
                    DISCOVERY_ID,
                    "--effort",
                    effort,
                    "--tools",
                    "",
                    "--setting-sources",
                    "",
                    "Return the word fixture.",
                ],
                capture_output=True,
                text=True,
                timeout=30,
                env=env,
            )
            captured = [
                request
                for request in _GatewayHandler.requests[before:]
                if request["path"].split("?", 1)[0] == "/v1/messages"
                and request["body"].get("model") == DISCOVERY_ID
            ]
            assert captured, f"Claude Code sent no Messages request for effort={effort}"
            observed = [request["body"].get("output_config") for request in captured]
            assert captured[-1]["body"]["output_config"]["effort"] == effort, observed
            headers = captured[-1]["headers"]
            assert headers.get("x-clud-gateway-token") == "fixture-token-canary"
            credential_headers = {
                name: value
                for name, value in headers.items()
                if name == "authorization"
                or name == "x-api-key"
                or name.endswith("-api-key")
                or name.endswith("-auth-token")
            }
            assert credential_headers, headers
            assert all(
                value
                in {
                    "fixture-api-key-canary",
                    "Bearer fixture-api-key-canary",
                }
                for value in credential_headers.values()
            ), credential_headers
        assert any(path.split("?", 1)[0] == "/v1/models" for path in _GatewayHandler.gets)
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
