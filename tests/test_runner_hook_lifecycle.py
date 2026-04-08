"""Tests for runner lifecycle hooks and Codex stop-hook emulation."""

from __future__ import annotations

import os
import unittest
from unittest.mock import MagicMock, patch

from clud.agent.completion import CompletionDetectionResult
from clud.agent.hooks import HookRegistrationSummary
from clud.agent.interfaces import LaunchPlan
from clud.agent.runner import run_agent
from clud.agent_args import AgentMode, Args
from clud.hooks import HookContext, HookEvent


class TestRunnerHookLifecycle(unittest.TestCase):
    """Tests for stop hook semantics in the main runner."""

    @staticmethod
    def _identity_command(cmd: list[str]) -> list[str]:
        return cmd

    def test_keyboard_interrupt_emits_agent_stop_once(self) -> None:
        args = Args(mode=AgentMode.DEFAULT, agent_backend="codex", no_skills=True)
        adapter = MagicMock()
        adapter.build_launch_plan.return_value = LaunchPlan(
            backend="codex",
            executable="codex",
            cwd=os.getcwd(),
            interactive=True,
        )
        recorded_events: list[HookEvent] = []

        def record_event(event: HookEvent, context: HookContext, hook_debug: bool = False) -> None:
            recorded_events.append(event)

        with (
            patch("clud.agent.runner.register_hooks_from_config", return_value=HookRegistrationSummary()),
            patch("clud.agent.runner._find_backend_executable", return_value="codex"),
            patch("clud.agent.runner.get_backend", return_value=adapter),
            patch("clud.agent.runner.run_claude_process", side_effect=KeyboardInterrupt()),
            patch("clud.agent.runner.trigger_hook_sync", side_effect=record_event),
            patch("clud.agent.runner._wrap_command_for_git_bash", side_effect=self._identity_command),
        ):
            result = run_agent(args)

        self.assertEqual(result, 130)
        self.assertEqual(recorded_events.count(HookEvent.AGENT_STOP), 1)

    def test_codex_stop_hooks_enable_idle_timeout_automatically(self) -> None:
        args = Args(mode=AgentMode.DEFAULT, agent_backend="codex", no_skills=True)
        adapter = MagicMock()
        adapter.build_launch_plan.return_value = LaunchPlan(
            backend="codex",
            executable="codex",
            cwd=os.getcwd(),
            interactive=True,
        )

        with (
            patch(
                "clud.agent.runner.register_hooks_from_config",
                return_value=HookRegistrationSummary(has_stop_hooks=True),
            ),
            patch("clud.agent.runner._find_backend_executable", return_value="codex"),
            patch("clud.agent.runner.get_backend", return_value=adapter),
            patch(
                "clud.agent.runner.detect_agent_completion",
                return_value=CompletionDetectionResult(idle_detected=True, returncode=0),
            ) as mock_detect,
            patch("clud.agent.runner.trigger_hook_sync"),
            patch("clud.agent.runner._wrap_command_for_git_bash", side_effect=self._identity_command),
        ):
            result = run_agent(args)

        self.assertEqual(result, 0)
        self.assertAlmostEqual(mock_detect.call_args.args[1], 3.0)


if __name__ == "__main__":
    unittest.main()
