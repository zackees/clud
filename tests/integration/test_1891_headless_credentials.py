"""#1891: API-key providers work on a headless Linux host with no keyring.

The real binary runs with no D-Bus session (so the Secret Service is
unreachable) and without the debug-only test vault, which is the state of an
SSH session on a headless server. The credential chain must then read the
owner-only fallback file, and the provider's env var as a last resort.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from typing import Any

import pytest

from .test_mock_agents import _run

pytestmark = [
    pytest.mark.integration,
    pytest.mark.skipif(
        not sys.platform.startswith("linux"),
        reason="macOS and Windows always have a native vault",
    ),
]

FILE_CANARY = "sk-or-v1-canary-file-1891"
ENV_CANARY = "sk-or-v1-canary-env-1891"


def _headless(mock_env: dict[str, str]) -> dict[str, str]:
    env = mock_env.copy()
    for name in (
        "DBUS_SESSION_BUS_ADDRESS",
        "CLUD_TEST_SECRET_STORE_DIR",
        "OPENROUTER_API_KEY",
        "DEEPSEEK_API_KEY",
        "KIMI_API_KEY",
    ):
        env.pop(name, None)
    # A runtime dir with no bus socket keeps the keyring crate from finding
    # the host's session bus.
    runtime = Path(env["HOME"]) / "run"
    runtime.mkdir(parents=True, exist_ok=True)
    env["XDG_RUNTIME_DIR"] = str(runtime)
    return env


def _openrouter_row(clud: Path, env: dict[str, str]) -> dict[str, Any]:
    result = _run(clud, "auth", "status", "--json", env=env)
    assert result.returncode == 0, result.stderr
    for canary in (FILE_CANARY, ENV_CANARY):
        assert canary not in result.stdout + result.stderr
    rows = json.loads(result.stdout)["providers"]
    return next(row for row in rows if row["provider"] == "openrouter")


def test_headless_host_reads_the_fallback_file(
    clud_binary: Path, mock_env: dict[str, str]
) -> None:
    env = _headless(mock_env)
    assert _openrouter_row(clud_binary, env)["status"] == "login_required"

    credentials = Path(env["HOME"]) / ".clud" / "credentials"
    credentials.mkdir(parents=True, mode=0o700)
    record = credentials / "clud_openrouter--api_key_v1.secret"
    record.write_text(FILE_CANARY, encoding="utf-8")
    os.chmod(record, 0o600)

    row = _openrouter_row(clud_binary, env)
    assert row["status"] == "configured"
    assert row["source"] == "file"


def test_headless_host_falls_back_to_the_env_var(
    clud_binary: Path, mock_env: dict[str, str]
) -> None:
    env = _headless(mock_env)
    env["OPENROUTER_API_KEY"] = ENV_CANARY

    row = _openrouter_row(clud_binary, env)
    assert row["status"] == "configured"
    assert row["source"] == "env"
    credentials = Path(env["HOME"]) / ".clud" / "credentials"
    assert not credentials.exists(), "env keys are never persisted"
