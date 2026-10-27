"""Integration tests for `clud serve` — the standalone API gateway (#2218).

`clud serve` is a durable, loopback, token-gated Anthropic Messages gateway
that reuses the unified gateway's route table but owns its own lifecycle. These
tests drive the real binary: the parse/refusal paths, and one live server that
an independent client reaches over the loopback listener.
"""

from __future__ import annotations

import re
import signal
import time
import urllib.error
import urllib.request
from pathlib import Path

import pytest

from tests import process

pytestmark = pytest.mark.integration

_TIMEOUT = 120
_ENDPOINT_RE = re.compile(r"endpoint=(http://[^\s]+)\s+token=(\S+)")


def _serve_env(mock_env: dict[str, str], tmp_path: Path) -> dict[str, str]:
    env = dict(mock_env)
    env["HOME"] = str(tmp_path)
    env["USERPROFILE"] = str(tmp_path)
    # A stored OpenRouter credential is all the served route needs; the env
    # fallback lets the test run without touching the developer's vault.
    env["OPENROUTER_API_KEY"] = "sk-or-v1-serve-gateway-test"
    return env


def _get(url: str, token: str | None) -> tuple[int, str]:
    request = urllib.request.Request(url)
    if token is not None:
        request.add_header("X-Clud-Gateway-Token", token)
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            return response.status, response.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as error:  # pragma: no cover - exercised on 401
        return error.code, error.read().decode("utf-8", "replace")


def test_serve_requires_a_provider(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    result = process.run(
        [str(clud_binary), "serve"],
        capture_output=True,
        text=True,
        timeout=_TIMEOUT,
        env=_serve_env(mock_env, tmp_path),
        cwd=tmp_path,
    )
    assert result.returncode == 2, result.stdout + result.stderr
    assert "select at least one provider" in result.stderr
    assert "#2218" in result.stderr


def test_serve_refuses_claude_until_the_credential_question_is_settled(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    result = process.run(
        [str(clud_binary), "serve", "--claude"],
        capture_output=True,
        text=True,
        timeout=_TIMEOUT,
        env=_serve_env(mock_env, tmp_path),
        cwd=tmp_path,
    )
    assert result.returncode == 2, result.stdout + result.stderr
    assert "--claude is not yet available" in result.stderr
    assert "#2218" in result.stderr


def test_serve_refuses_a_non_loopback_bind(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    result = process.run(
        [str(clud_binary), "serve", "--openrouter", "--bind", "0.0.0.0"],
        capture_output=True,
        text=True,
        timeout=_TIMEOUT,
        env=_serve_env(mock_env, tmp_path),
        cwd=tmp_path,
    )
    assert result.returncode == 2, result.stdout + result.stderr
    assert "loopback only" in result.stderr


def test_serve_serves_models_and_requires_the_client_token(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    """A live server: an independent client lists models, and a missing token is refused."""
    child = process.Popen(
        [str(clud_binary), "serve", "--openrouter"],
        stdout=process.PIPE,
        stderr=process.PIPE,
        text=True,
        env=_serve_env(mock_env, tmp_path),
        cwd=tmp_path,
    )
    buffer = ""
    endpoint = ""
    token = ""
    returncode: int | None = None
    deadline = time.monotonic() + _TIMEOUT
    try:
        while time.monotonic() < deadline:
            try:
                line = child.next_stdout(timeout=0.5)
            except TimeoutError:
                if child.poll() is not None:
                    break
                continue
            text = (
                line.decode("utf-8", "replace")
                if isinstance(line, bytes)
                else str(line)
            )
            buffer += text
            match = _ENDPOINT_RE.search(buffer)
            if match:
                endpoint, token = match.group(1), match.group(2)
                break
            if child.poll() is not None:
                break
        assert endpoint, f"`clud serve` never advertised an endpoint.\n{buffer}"
        assert token, f"`clud serve` advertised no client token.\n{buffer}"

        status, body = _get(f"{endpoint}/v1/models", token)
        assert status == 200, f"authenticated /v1/models failed: {status}\n{body[:2000]}"
        assert "openrouter" in body.lower(), body[:2000]

        unauth, _ = _get(f"{endpoint}/v1/models", None)
        assert unauth == 401, f"an unauthenticated client must be refused, got {unauth}"
    finally:
        try:
            child.send_signal(signal.SIGINT)
        except (NotImplementedError, OSError):  # pragma: no cover - platform fallback
            child.terminate()
        try:
            returncode = child.wait(timeout=15)
        except process.TimeoutExpired:  # pragma: no cover - shutdown regression
            child.kill()
            returncode = child.wait(timeout=5)

    # A clean exit code after SIGINT is the shutdown guarantee; the banner is
    # the last line this server writes before it blocks on the signal handler.
    assert returncode == 0, f"Ctrl+C must shut the server down cleanly, got {returncode}"
