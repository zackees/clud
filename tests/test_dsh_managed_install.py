"""#1829 clean-room acceptance: clud installs DeepSeek Harness and drives it.

Runs only with ``DSH_MANAGED_ACCEPTANCE=1``, a compiled candidate
(``CLUD_TEST_BINARY``) and npm on the host, which the installer-check
``candidate-dsh`` lane provides. Each test starts from an empty HOME. The OpenRouter round trip boots the real managed
dsh against a local mock of OpenRouter's Anthropic Messages endpoint, so it
proves the key, model and route without a paid request.
"""

from __future__ import annotations

import json
import os
import shutil
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

from tests.process import run as run_process

DSH_VERSION = "0.2.0-rc.2"
CANARY = "sk-or-canary-managed-1829"
VAULT_FILE = "clud_openrouter--api_key_v1.secret"
MODEL = "~anthropic/claude-sonnet-latest"

# The first test pays for a full npm download of dsh and a Node runtime.
pytestmark = pytest.mark.timeout(1800)


def _clud() -> Path:
    if os.environ.get("DSH_MANAGED_ACCEPTANCE") != "1":
        pytest.skip("set DSH_MANAGED_ACCEPTANCE=1: the managed install downloads from npm")
    candidate = os.environ.get("CLUD_TEST_BINARY")
    if not candidate or not Path(candidate).is_file():
        pytest.skip("compiled clud candidate is required")
    if shutil.which("npm") is None:
        pytest.skip("npm is required for the managed dsh install")
    return Path(candidate)


@pytest.fixture(scope="module")
def installed(tmp_path_factory: pytest.TempPathFactory) -> dict[str, str]:
    clud = _clud()
    home = tmp_path_factory.mktemp("home")
    vault = tmp_path_factory.mktemp("vault")
    (vault / VAULT_FILE).write_text(CANARY, encoding="utf-8")
    env = os.environ.copy()
    for name in ("DEEPSEEK_API_KEY", "OPENROUTER_API_KEY"):
        env.pop(name, None)
    env["HOME"] = str(home)
    if sys.platform == "win32":
        env["USERPROFILE"] = str(home)
        env.pop("LOCALAPPDATA", None)
    env["CLUD_INTEGRATION_TESTS"] = "1"
    env["CLUD_TEST_SECRET_STORE_DIR"] = str(vault)
    env["DSH_HOME"] = str(home / "dsh-home")
    env["CLUD_BIN"] = str(clud)
    # A user-managed dsh on PATH would rightly win over the managed one.
    env["PATH"] = os.pathsep.join(
        entry
        for entry in env.get("PATH", "").split(os.pathsep)
        if not any((Path(entry) / name).exists() for name in ("dsh", "dsh.cmd"))
    )
    first = _run(env, "dsh-update", timeout=1200)
    assert first.returncode == 0, first.stdout + first.stderr
    assert f"Installed DeepSeek Harness {DSH_VERSION}" in first.stdout
    return env


def _run(env: dict[str, str], *args: str, timeout: int = 300):
    return run_process(
        [env["CLUD_BIN"], *args],
        env=env,
        capture_output=True,
        text=True,
        timeout=timeout,
    )


def _prefix(env: dict[str, str]) -> Path:
    return Path(env["HOME"]) / ".clud" / "harnesses" / "dsh" / DSH_VERSION


def test_install_is_private_pinned_and_idempotent(installed: dict[str, str]) -> None:
    prefix = _prefix(installed)
    launcher = prefix / "node_modules" / ".bin" / ("dsh.cmd" if sys.platform == "win32" else "dsh")
    assert launcher.is_file()
    assert (prefix / "node_modules" / "node").is_dir(), "private Node runtime"
    staging = [p.name for p in prefix.parent.iterdir() if ".staging-" in p.name]
    assert staging == []
    again = _run(installed, "dsh-update")
    assert again.returncode == 0, again.stderr
    assert "already installed" in again.stdout


def test_launch_plan_resolves_the_managed_dsh(installed: dict[str, str]) -> None:
    result = _run(installed, "--openrouter", "--harness", "dsh", "--dry-run", "-p", "hi")
    assert result.returncode == 0, result.stderr
    command = json.loads(result.stdout)["command"]
    assert Path(command[0]).parent.parent.parent == _prefix(installed)


class _MockOpenRouter(BaseHTTPRequestHandler):
    requests: list[dict] = []

    def do_GET(self) -> None:  # noqa: N802 - http.server API
        # clud's credential preflight probe (`/api/v1/key`).
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(b'{"data": {}}')

    def do_POST(self) -> None:  # noqa: N802 - http.server API
        length = int(self.headers.get("content-length", 0))
        body = json.loads(self.rfile.read(length) or b"{}")
        self.requests.append(
            {"path": self.path, "key": self.headers.get("x-api-key"), "model": body.get("model")}
        )
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        model = body.get("model")
        events = [
            (
                "message_start",
                {
                    "type": "message_start",
                    "message": {
                        "id": "msg_1",
                        "type": "message",
                        "role": "assistant",
                        "model": model,
                        "content": [],
                        "stop_reason": None,
                        "stop_sequence": None,
                        "usage": {"input_tokens": 1, "output_tokens": 0},
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
                    "delta": {"type": "text_delta", "text": "OK"},
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
        ]
        for name, data in events:
            self.wfile.write(f"event: {name}\ndata: {json.dumps(data)}\n\n".encode())

    def log_message(self, *args: object) -> None:
        return


def test_openrouter_round_trip_through_the_real_dsh(installed: dict[str, str]) -> None:
    server = ThreadingHTTPServer(("127.0.0.1", 0), _MockOpenRouter)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        env = dict(installed)
        base = f"http://127.0.0.1:{server.server_port}/api"
        env["CLUD_TEST_DSH_OPENROUTER_BASE_URL"] = base
        env["CLUD_TEST_CREDENTIAL_PROBE_URL"] = f"{base}/v1/key"
        result = _run(
            env,
            "--openrouter",
            "--harness",
            "dsh",
            "--model",
            MODEL,
            "-p",
            "reply with OK",
            timeout=600,
        )
    finally:
        server.shutdown()
    assert result.returncode == 0, result.stdout + result.stderr
    assert "OK" in result.stdout
    assert _MockOpenRouter.requests, "dsh never reached the OpenRouter endpoint"
    first = _MockOpenRouter.requests[0]
    assert first["path"].startswith("/api/v1/messages")
    assert first["key"] == CANARY
    assert first["model"] == MODEL
    assert CANARY not in result.stdout + result.stderr
