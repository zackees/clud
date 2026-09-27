"""Dry-run launch delivery of the generated deletion policy (#1461)."""

from __future__ import annotations

import json
import os
from pathlib import Path

from tests import process


def plan(tmp_path: Path, *extra: str, backend: str = "claude") -> list[str]:
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    env = os.environ.copy()
    env.update(HOME=str(home), USERPROFILE=str(home))
    binary = Path(os.environ.get("CLUD_TEST_BINARY", "/build/target/debug/clud"))
    result = process.run(
        [str(binary), "--dry-run", f"--{backend}", "--no-daemon", "-p", "hello", *extra],
        cwd=str(tmp_path), env=env, capture_output=True, text=True, timeout=30,
    )
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)["command"]


def option(command: list[str], name: str) -> str:
    indexes = [index for index, word in enumerate(command) if word == name]
    assert len(indexes) == 1, command
    return command[indexes[0] + 1]


def test_claude_dry_run_contains_generated_deny_settings(tmp_path: Path) -> None:
    command = plan(tmp_path)
    settings = json.loads(option(command, "--settings"))
    denies = settings["permissions"]["deny"]
    for remover in ("rm", "rmdir", "unlink"):
        assert f"Bash({remover} *)" in denies


def test_claude_dry_run_merges_user_settings_without_duplicates(tmp_path: Path) -> None:
    existing = "Bash(" + "rm" + " *)"
    user = {"permissions": {"deny": ["Bash(false *)", existing]}}
    command = plan(tmp_path, "--", "--settings", json.dumps(user))
    settings = json.loads(option(command, "--settings"))
    denies = settings["permissions"]["deny"]
    assert "Bash(false *)" in denies
    assert denies.count(existing) == 1


def test_claude_dry_run_combines_user_and_generated_instructions(tmp_path: Path) -> None:
    command = plan(tmp_path, "--", "--append-system-prompt", "USER INSTRUCTIONS")
    paragraph = option(command, "--append-system-prompt")
    assert "safe-rm" in paragraph
    assert "USER INSTRUCTIONS" in paragraph


def codex_overrides(command: list[str]) -> list[str]:
    return [command[index + 1] for index, word in enumerate(command[:-1]) if word == "-c"]


def test_codex_dry_run_carries_trusted_hook_and_instructions(tmp_path: Path) -> None:
    command = plan(tmp_path, backend="codex")
    overrides = codex_overrides(command)
    assert len([item for item in overrides if item.startswith("hooks.PreToolUse=")]) == 1
    state = [item for item in overrides if item.startswith("hooks.state=")]
    assert len(state) == 1
    assert "trusted_hash=\"sha256:" in state[0]
    assert "/<session-flags>/config.toml:pre_tool_use:0:0" in state[0]
    instructions = [item for item in overrides if item.startswith("developer_instructions=")]
    assert len(instructions) == 1
    assert "safe-rm" in instructions[0]
    assert "--dangerously-bypass-hook-trust" not in command
    assert not (tmp_path / "home/.codex").exists()


def test_codex_safe_launch_still_has_policy(tmp_path: Path) -> None:
    command = plan(tmp_path, "--safe", backend="codex")
    overrides = codex_overrides(command)
    assert any(item.startswith("hooks.PreToolUse=") for item in overrides)
    assert any("safe-rm" in item for item in overrides)
    assert "--dangerously-bypass-approvals-and-sandbox" not in command


def test_codex_user_developer_instructions_are_preserved(tmp_path: Path) -> None:
    config = tmp_path / "home/.codex"
    config.mkdir(parents=True)
    (config / "config.toml").write_text(
        'developer_instructions = "USER CONFIG INSTRUCTIONS"\n', encoding="utf-8"
    )
    command = plan(tmp_path, backend="codex")
    values = [
        item for item in codex_overrides(command) if item.startswith("developer_instructions=")
    ]
    assert len(values) == 1
    assert "USER CONFIG INSTRUCTIONS" in values[0]
    assert "safe-rm" in values[0]
    command = plan(
        tmp_path, "--", "-c", 'developer_instructions="INLINE INSTRUCTIONS"', backend="codex"
    )
    values = [
        item for item in codex_overrides(command) if item.startswith("developer_instructions=")
    ]
    assert len(values) == 1
    assert "INLINE INSTRUCTIONS" in values[0]
    assert "safe-rm" in values[0]
