"""Persistent OpenRouter privacy across independent launcher processes (#1998)."""

from __future__ import annotations

import json
import os
from pathlib import Path

from tests import process


def test_saved_openrouter_zdr_survives_fresh_processes_and_directories(tmp_path: Path) -> None:
    binary = os.environ.get("CLUD_TEST_BINARY")
    assert binary, "run with the built CI bundle"
    home = tmp_path / "home"
    settings = home / ".clud" / "settings.json"
    settings.parent.mkdir(parents=True)
    settings.write_text(
        json.dumps({"providers": {"openrouter": {"zdr": True}}, "unrelated": "preserved"}),
        encoding="utf-8",
    )
    original = settings.read_bytes()
    env = {key: value for key, value in os.environ.items() if not key.startswith("CLUD_")}
    env.update(
        {
            "HOME": str(home),
            "USERPROFILE": str(home),
            "LOCALAPPDATA": str(home / "local-app-data"),
            "XDG_STATE_HOME": str(home / ".local" / "state"),
            "XDG_CACHE_HOME": str(home / ".cache"),
            "CLUD_HOOK_HOME": str(home),
            "CLUD_DAEMON_STATE_DIR": str(tmp_path / "state"),
            "CLUD_NO_UNLOCK": "1",
            "CLUD_SERVER_SETTINGS": "0",
        }
    )
    routes = [
        ["--openrouter", "--model", "~anthropic/claude-sonnet-latest"],
        ["--unified", "--model", "openrouter-claude-sonnet"],
    ]
    for index, route in enumerate(routes):
        cwd = tmp_path / f"repo-{index}"
        cwd.mkdir()
        result = process.run(
            [binary, "--dry-run", *route, "-p", "hello"],
            cwd=cwd,
            env=env,
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert result.returncode == 0, result.stdout + result.stderr
        plan = json.loads(result.stdout)
        routing = plan["route"]["upstream_routing"] or {}
        assert routing.get("zdr") is True, plan["route"]
        assert settings.read_bytes() == original, "dry-run must not change saved settings"
