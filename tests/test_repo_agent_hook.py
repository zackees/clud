"""Tests for the repo-local agent hook configuration."""

from __future__ import annotations

import os
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

from clud.agent.hooks import register_hooks_from_config, trigger_hook_sync
from clud.hooks import HookContext, HookEvent, reset_hook_manager


class TestRepoAgentHook(unittest.TestCase):
    """Verify the checked-in repo hook loads and executes correctly."""

    def tearDown(self) -> None:
        """Reset the global hook manager after each test."""
        reset_hook_manager()

    def test_repo_start_hook_is_invoked_via_agent_start_event(self) -> None:
        repo_root = Path(__file__).resolve().parents[1]

        with TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            output_path = tmp_path / "agent-start-hook-output.txt"
            context = HookContext(
                event=HookEvent.AGENT_START,
                instance_id="test-instance",
                session_id="test-session",
                client_type="codex",
                client_id="codex",
                metadata={"cwd": str(tmp_path)},
            )

            with patch.dict(os.environ, {"CLUD_AGENT_HOOK_OUTPUT": str(output_path)}, clear=False):
                summary = register_hooks_from_config(cwd=repo_root)
                trigger_hook_sync(HookEvent.AGENT_START, context)

            self.assertTrue(summary.has_start_hooks)
            self.assertTrue(output_path.exists())
            self.assertEqual(
                output_path.read_text(encoding="utf-8"),
                f"event=Start\ncwd={tmp_path}\n",
            )

    def test_repo_stop_hook_is_invoked_via_post_execution_event(self) -> None:
        repo_root = Path(__file__).resolve().parents[1]

        with TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            output_path = tmp_path / "agent-stop-hook-output.txt"
            context = HookContext(
                event=HookEvent.POST_EXECUTION,
                instance_id="test-instance",
                session_id="test-session",
                client_type="codex",
                client_id="codex",
                metadata={"cwd": str(tmp_path)},
            )

            with patch.dict(os.environ, {"CLUD_AGENT_HOOK_OUTPUT": str(output_path)}, clear=False):
                summary = register_hooks_from_config(cwd=repo_root)
                trigger_hook_sync(HookEvent.POST_EXECUTION, context)

            self.assertTrue(summary.has_post_execution_hooks)
            self.assertTrue(output_path.exists())
            self.assertEqual(
                output_path.read_text(encoding="utf-8"),
                f"event=Stop\ncwd={tmp_path}\n",
            )

    def test_repo_session_end_hook_is_invoked_via_agent_stop_event(self) -> None:
        repo_root = Path(__file__).resolve().parents[1]

        with TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            output_path = tmp_path / "agent-session-end-hook-output.txt"
            context = HookContext(
                event=HookEvent.AGENT_STOP,
                instance_id="test-instance",
                session_id="test-session",
                client_type="codex",
                client_id="codex",
                metadata={"cwd": str(tmp_path)},
            )

            with patch.dict(os.environ, {"CLUD_AGENT_HOOK_OUTPUT": str(output_path)}, clear=False):
                summary = register_hooks_from_config(cwd=repo_root)
                trigger_hook_sync(HookEvent.AGENT_STOP, context)

            self.assertTrue(summary.has_session_end_hooks)
            self.assertTrue(output_path.exists())
            self.assertEqual(
                output_path.read_text(encoding="utf-8"),
                f"event=SessionEnd\ncwd={tmp_path}\n",
            )


if __name__ == "__main__":
    unittest.main()
