"""Tests for command-based hook handlers."""

from __future__ import annotations

import unittest
from datetime import datetime
from pathlib import Path
from unittest.mock import MagicMock, patch

from clud.hooks import HookContext, HookEvent
from clud.hooks.command import CommandHookHandler, CommandHookSpec


class TestCommandHookHandler(unittest.IsolatedAsyncioTestCase):
    """Tests for shell-command hook execution."""

    async def test_command_handler_uses_context_cwd_and_env(self) -> None:
        handler = CommandHookHandler([CommandHookSpec(event_name="Stop", command="bash lint")], timeout_seconds=5.0)
        context = HookContext(
            event=HookEvent.POST_EXECUTION,
            instance_id="instance-1",
            session_id="session-1",
            client_type="cli",
            client_id="standalone",
            timestamp=datetime.now(),
            metadata={
                "backend": "codex",
                "cwd": str(Path("C:/tmp/project")),
                "reason": "idle_detected",
                "returncode": 0,
                "idle_detected": True,
            },
        )

        with patch("clud.hooks.command.subprocess.run") as mock_run:
            mock_run.return_value = MagicMock(returncode=0, stdout="", stderr="")
            await handler.handle(context)

        _, kwargs = mock_run.call_args
        self.assertEqual(Path(kwargs["cwd"]), Path("C:/tmp/project"))
        self.assertEqual(Path(kwargs["env"]["CLAUDE_PROJECT_DIR"]), Path("C:/tmp/project"))
        self.assertEqual(kwargs["env"]["CLUD_BACKEND"], "codex")
        self.assertEqual(kwargs["env"]["CLUD_STOP_REASON"], "idle_detected")
        self.assertEqual(kwargs["env"]["CLUD_HOOK_EVENT"], "Stop")


if __name__ == "__main__":
    unittest.main()
