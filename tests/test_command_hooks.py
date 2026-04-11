"""Tests for command-based hook handlers."""

from __future__ import annotations

import subprocess
import unittest
from datetime import datetime
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import MagicMock, patch

from clud.hooks import HookContext, HookEvent
from clud.hooks.command import (
    POST_HOOK_FAILURE_FILENAME,
    CommandHookHandler,
    CommandHookResult,
    CommandHookSpec,
    run_command_hook,
    write_failure_artifact,
)
from clud.util.process import run_with_input_detached


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

        with patch("clud.hooks.command.run_with_input_detached") as mock_run:
            mock_run.return_value = MagicMock(returncode=0, stdout="", stderr="")
            await handler.handle(context)

        _, kwargs = mock_run.call_args
        self.assertEqual(Path(kwargs["cwd"]), Path("C:/tmp/project"))
        self.assertEqual(Path(kwargs["env"]["CLAUDE_PROJECT_DIR"]), Path("C:/tmp/project"))
        self.assertEqual(kwargs["env"]["CLUD_BACKEND"], "codex")
        self.assertEqual(kwargs["env"]["CLUD_STOP_REASON"], "idle_detected")
        self.assertEqual(kwargs["env"]["CLUD_HOOK_EVENT"], "Stop")

    async def test_run_with_input_detached_centralizes_devnull_stdin(self) -> None:
        with patch("clud.util.process.RunningProcess") as mock_process_cls:
            mock_process = MagicMock()
            mock_process.wait.return_value = 0
            mock_process.stdout = ""
            mock_process_cls.return_value = mock_process
            run_with_input_detached(
                "python -c \"print('ok')\"",
                shell=True,
                cwd=Path("C:/tmp/project"),
                env={"EXAMPLE": "1"},
                timeout=5.0,
            )

        _, kwargs = mock_process_cls.call_args
        self.assertEqual(kwargs["stdin"], subprocess.DEVNULL)
        self.assertEqual(kwargs["shell"], True)
        self.assertEqual(kwargs["cwd"], Path("C:/tmp/project"))
        self.assertEqual(kwargs["env"], {"EXAMPLE": "1"})
        self.assertEqual(kwargs["timeout"], 5)
        mock_process.wait.assert_called_once_with()

    async def test_run_command_hook_captures_nonzero_result(self) -> None:
        spec = CommandHookSpec(event_name="Stop", command="python -c \"import sys; print('lint bad'); print('stderr bad', file=sys.stderr); sys.exit(7)\"")
        context = HookContext(
            event=HookEvent.POST_EXECUTION,
            instance_id="instance-1",
            session_id="session-1",
            client_type="cli",
            client_id="standalone",
            timestamp=datetime.now(),
            metadata={"cwd": str(Path.cwd())},
        )

        result = run_command_hook(spec, context, timeout_seconds=5.0)

        self.assertEqual(result.returncode, 7)
        self.assertIn("lint bad", result.stdout)
        self.assertIn("stderr bad", result.stdout)
        self.assertEqual(result.stderr, "")
        self.assertTrue(result.failed)

    async def test_write_failure_artifact_persists_output(self) -> None:
        with TemporaryDirectory() as tmp:
            result = CommandHookResult(
                spec=CommandHookSpec(event_name="Stop", command="bash lint", source_path="settings.local.json"),
                cwd=Path(tmp),
                returncode=2,
                stdout="lint output\n",
                stderr="lint stderr\n",
            )

            artifact = write_failure_artifact(result)

            self.assertEqual(artifact, Path(tmp) / POST_HOOK_FAILURE_FILENAME)
            content = artifact.read_text(encoding="utf-8")
            self.assertIn("Post-edit hook failure", content)
            self.assertIn("Command: bash lint", content)
            self.assertIn("Return code: 2", content)
            self.assertIn("lint output", content)
            self.assertIn("lint stderr", content)


if __name__ == "__main__":
    unittest.main()
