"""Settings operations and incidental mentions through the real CLI (#1999)."""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

import pytest

from tests import process


def _world(tmp_path: Path) -> tuple[str, dict[str, str], Path, Path]:
    binary = os.environ.get("CLUD_TEST_BINARY")
    assert binary, "run with the built CI bundle"
    home = tmp_path / "home"
    settings = home / ".clud" / "settings.json"
    settings.parent.mkdir(parents=True)
    settings.write_text(
        json.dumps({"safe_rm": {"extra_roots": []}, "unrelated": "preserved"}),
        encoding="utf-8",
    )
    repo = tmp_path / "repo"
    repo.mkdir()
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
    return binary, env, repo, settings


def _scan(
    world: tuple[str, dict[str, str], Path, Path], command: str
) -> process.CompletedProcess[str]:
    binary, env, repo, _settings = world
    return process.run(
        [binary, "__cmd-scan", "--event", "PreToolUse"],
        input=json.dumps(
            {
                "hook_event_name": "PreToolUse",
                "session_id": "settings-authorization-1999",
                "cwd": str(repo),
                "tool_name": "Bash",
                "tool_input": {"command": command},
            }
        ),
        cwd=repo,
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )


@pytest.mark.parametrize(
    "command",
    [
        "gh issue list --repo zackees/clud --search 'safe_rm.extra_roots ~/.clud/settings.json'",
        "cat > issue-draft.md <<'EOF'\nDocument ~/.clud/settings.json and safe_rm.extra_roots.\nEOF",
    ],
    ids=["read-only-forge-search", "scratch-heredoc-data"],
)
def test_harmless_settings_mentions_do_not_need_authority(tmp_path: Path, command: str) -> None:
    world = _world(tmp_path)
    original = world[3].read_bytes()
    result = _scan(world, command)
    assert result.returncode == 0, result.stdout + result.stderr
    assert world[3].read_bytes() == original


@pytest.mark.skipif(sys.platform != "linux", reason="Linux missing-desktop refusal")
def test_controlled_settings_change_refuses_without_a_user_approval_surface(tmp_path: Path) -> None:
    binary, env, repo, settings = _world(tmp_path)
    original = settings.read_bytes()
    for key in ["DISPLAY", "WAYLAND_DISPLAY"]:
        env.pop(key, None)
    result = process.run(
        [binary, "settings", "set", "/web_term/enabled", "false", "--reason", "User requested it"],
        cwd=repo,
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 1, result.stdout + result.stderr
    assert "approval" in result.stderr.lower(), result.stderr
    assert "desktop" in result.stderr.lower(), result.stderr
    assert settings.read_bytes() == original
