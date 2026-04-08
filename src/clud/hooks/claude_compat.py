"""Claude hook compatibility helpers.

This module loads a small compatible subset of Claude Code's hook settings
for reuse by clud when running either Claude or Codex.
"""

from __future__ import annotations

import json
import os
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, cast

from clud.util.json_loading import load_json_file_permissive

from .command import CommandHookSpec

DEFAULT_CODEX_STOP_IDLE_TIMEOUT = 3.0


def _command_spec_list() -> list[CommandHookSpec]:
    return []


@dataclass(slots=True)
class ClaudeCompatHooks:
    """Normalized hook commands loaded from Claude settings files."""

    stop: list[CommandHookSpec] = field(default_factory=_command_spec_list)
    session_end: list[CommandHookSpec] = field(default_factory=_command_spec_list)

    @property
    def has_stop(self) -> bool:
        return bool(self.stop)

    @property
    def has_session_end(self) -> bool:
        return bool(self.session_end)


def load_claude_compat_hooks(cwd: Path | None = None) -> ClaudeCompatHooks:
    """Load Stop/SessionEnd commands from Claude settings JSON files."""
    cwd = cwd or Path.cwd()
    hooks = ClaudeCompatHooks()
    seen: set[tuple[str, str]] = set()

    for path in _settings_paths(cwd):
        for event_name, commands in _load_commands_from_settings(path).items():
            target = hooks.stop if event_name == "Stop" else hooks.session_end
            for command in commands:
                key = (event_name, command)
                if key in seen:
                    continue
                seen.add(key)
                target.append(CommandHookSpec(event_name=event_name, command=command, source_path=str(path)))

    return hooks


def get_codex_stop_hook_idle_timeout() -> float:
    """Return the idle timeout used to emulate Claude's Stop hook for Codex."""
    raw = os.environ.get("CLUD_CODEX_STOP_HOOK_IDLE_TIMEOUT")
    if raw is None:
        return DEFAULT_CODEX_STOP_IDLE_TIMEOUT
    try:
        parsed = float(raw)
    except ValueError:
        return DEFAULT_CODEX_STOP_IDLE_TIMEOUT
    return parsed if parsed > 0 else DEFAULT_CODEX_STOP_IDLE_TIMEOUT


def _settings_paths(cwd: Path) -> list[Path]:
    return [
        Path.home() / ".claude" / "settings.json",
        cwd / ".claude" / "settings.json",
        cwd / ".claude" / "settings.local.json",
    ]


def _load_commands_from_settings(path: Path) -> dict[str, list[str]]:
    try:
        data = load_json_file_permissive(path)
    except (FileNotFoundError, OSError, json.JSONDecodeError):
        return {"Stop": [], "SessionEnd": []}

    data_dict = _as_mapping(data)
    if data_dict is None:
        return {"Stop": [], "SessionEnd": []}

    raw_hooks = _as_mapping(data_dict.get("hooks"))
    if raw_hooks is None:
        return {"Stop": [], "SessionEnd": []}

    return {
        "Stop": _extract_commands(raw_hooks.get("Stop")),
        "SessionEnd": _extract_commands(raw_hooks.get("SessionEnd")),
    }


def _extract_commands(value: Any) -> list[str]:
    commands: list[str] = []

    def walk(node: Any) -> None:
        if node is None:
            return
        if isinstance(node, str):
            stripped = node.strip()
            if stripped:
                commands.append(stripped)
            return
        if isinstance(node, list):
            for item in cast(list[Any], node):
                walk(item)
            return
        node_dict = _as_mapping(node)
        if node_dict is not None:
            node_type = node_dict.get("type")
            command = node_dict.get("command")
            if (node_type is None or node_type == "command") and isinstance(command, str):
                walk(command)
            if "hooks" in node_dict:
                walk(node_dict["hooks"])
            return

    walk(value)
    return commands


def _as_mapping(value: Any) -> dict[str, Any] | None:
    if isinstance(value, dict):
        return cast(dict[str, Any], value)
    return None


__all__ = [
    "ClaudeCompatHooks",
    "DEFAULT_CODEX_STOP_IDLE_TIMEOUT",
    "get_codex_stop_hook_idle_timeout",
    "load_claude_compat_hooks",
]
