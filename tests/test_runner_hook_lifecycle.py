"""Tests for runner lifecycle hooks and Codex Claude-hook emulation."""

from __future__ import annotations

import json
import os
import unittest
from io import StringIO
from pathlib import Path
from tempfile import TemporaryDirectory
from typing import cast
from unittest.mock import MagicMock, patch

from clud.agent.hooks import HookRegistrationSummary
from clud.agent.interfaces import LaunchPlan
from clud.agent.process_launcher import PtySessionResult
from clud.agent.runner import run_agent
from clud.agent_args import AgentMode, Args
from clud.hooks import HookContext, HookEvent, reset_hook_manager


class TestRunnerHookLifecycle(unittest.TestCase):
    """Tests for stop hook semantics in the main runner."""

    def tearDown(self) -> None:
        reset_hook_manager()

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
            patch("sys.stdin.isatty", return_value=True),
        ):
            result = run_agent(args)

        self.assertEqual(result, 130)
        self.assertEqual(recorded_events.count(HookEvent.AGENT_STOP), 1)

    def test_codex_interactive_always_enables_idle_timeout_pty_launch(self) -> None:
        args = Args(mode=AgentMode.DEFAULT, agent_backend="codex", no_skills=True)
        adapter = MagicMock()
        adapter.build_launch_plan.return_value = LaunchPlan(
            backend="codex",
            executable="codex",
            cwd=os.getcwd(),
            interactive=True,
        )

        with (
            patch("clud.agent.runner.register_hooks_from_config", return_value=HookRegistrationSummary()),
            patch("clud.agent.runner.get_codex_stop_hook_idle_timeout", return_value=10.0),
            patch("clud.agent.runner._find_backend_executable", return_value="codex"),
            patch("clud.agent.runner.get_backend", return_value=adapter),
            patch("clud.agent.runner.run_claude_process", return_value=0) as mock_launch,
            patch("clud.agent.runner.trigger_hook_sync"),
            patch("clud.agent.runner._wrap_command_for_git_bash", side_effect=self._identity_command),
            patch("sys.stdin.isatty", return_value=True),
        ):
            result = run_agent(args)

        self.assertEqual(result, 0)
        mock_launch.assert_called_once()
        self.assertEqual(mock_launch.call_args.kwargs["idle_timeout"], 10.0)
        self.assertIn("on_idle", mock_launch.call_args.kwargs)

    def test_codex_inline_stop_hooks_skip_generic_post_execution_registration(self) -> None:
        args = Args(mode=AgentMode.DEFAULT, agent_backend="codex", no_skills=True)
        adapter = MagicMock()
        adapter.build_launch_plan.return_value = LaunchPlan(
            backend="codex",
            executable="codex",
            cwd=os.getcwd(),
            interactive=True,
        )

        with (
            patch("clud.agent.runner.load_claude_compat_hooks", return_value=MagicMock(stop=[MagicMock()], start=[], session_end=[])),
            patch("clud.agent.runner.register_hooks_from_config", return_value=HookRegistrationSummary(has_post_execution_hooks=True)) as mock_register,
            patch("clud.agent.runner.get_codex_stop_hook_idle_timeout", return_value=10.0),
            patch("clud.agent.runner._find_backend_executable", return_value="codex"),
            patch("clud.agent.runner.get_backend", return_value=adapter),
            patch("clud.agent.runner.run_claude_process", return_value=0),
            patch("clud.agent.runner.trigger_hook_sync"),
            patch("clud.agent.runner._wrap_command_for_git_bash", side_effect=self._identity_command),
            patch("sys.stdin.isatty", return_value=True),
        ):
            result = run_agent(args)

        self.assertEqual(result, 0)
        self.assertFalse(mock_register.call_args.kwargs["register_compat_stop"])

    def test_codex_inline_idle_callback_runs_post_execution_without_final_duplicate(self) -> None:
        args = Args(mode=AgentMode.DEFAULT, agent_backend="codex", no_skills=True)
        adapter = MagicMock()
        adapter.build_launch_plan.return_value = LaunchPlan(
            backend="codex",
            executable="codex",
            cwd=os.getcwd(),
            interactive=True,
        )
        compat_hook = MagicMock()
        compat_hook.event_name = "Stop"
        compat_hook.command = "echo stop"
        compat_hook.source_path = None
        events: list[HookEvent] = []

        def record_event(event: HookEvent, context: HookContext, hook_debug: bool = False) -> None:
            events.append(event)

        def fake_run_claude_process(*_args: object, **kwargs: object) -> PtySessionResult:
            on_idle = kwargs["on_idle"]
            assert callable(on_idle)
            self.assertEqual(on_idle(), "hook output")
            return PtySessionResult(returncode=0, idle_event_count=1)

        with (
            patch("clud.agent.runner.load_claude_compat_hooks", return_value=MagicMock(stop=[compat_hook], start=[], session_end=[])),
            patch("clud.agent.runner.register_hooks_from_config", return_value=HookRegistrationSummary(has_post_execution_hooks=True)),
            patch("clud.agent.runner.get_codex_stop_hook_idle_timeout", return_value=10.0),
            patch("clud.agent.runner._find_backend_executable", return_value="codex"),
            patch("clud.agent.runner.get_backend", return_value=adapter),
            patch("clud.agent.runner.run_claude_process", side_effect=fake_run_claude_process),
            patch("clud.agent.runner.trigger_hook_sync", side_effect=record_event),
            patch("clud.agent.runner.run_command_hook", return_value=MagicMock(failed=False, stdout="hook output", stderr="")),
            patch("clud.agent.runner._wrap_command_for_git_bash", side_effect=self._identity_command),
            patch("sys.stdin.isatty", return_value=True),
        ):
            result = run_agent(args)

        self.assertEqual(result, 0)
        self.assertEqual(events.count(HookEvent.POST_EXECUTION), 1)

    def test_codex_inline_idle_callback_sanitizes_hook_output_before_injection(self) -> None:
        args = Args(mode=AgentMode.DEFAULT, agent_backend="codex", no_skills=True)
        adapter = MagicMock()
        adapter.build_launch_plan.return_value = LaunchPlan(
            backend="codex",
            executable="codex",
            cwd=os.getcwd(),
            interactive=True,
        )
        compat_hook = MagicMock()
        compat_hook.event_name = "Stop"
        compat_hook.command = "echo stop"
        compat_hook.source_path = None

        def fake_run_claude_process(*_args: object, **kwargs: object) -> PtySessionResult:
            on_idle = kwargs["on_idle"]
            assert callable(on_idle)
            self.assertEqual(on_idle(), "hook outputwith ansi")
            return PtySessionResult(returncode=0, idle_event_count=1)

        with (
            patch("clud.agent.runner.load_claude_compat_hooks", return_value=MagicMock(stop=[compat_hook], start=[], session_end=[])),
            patch("clud.agent.runner.register_hooks_from_config", return_value=HookRegistrationSummary(has_post_execution_hooks=True)),
            patch("clud.agent.runner.get_codex_stop_hook_idle_timeout", return_value=10.0),
            patch("clud.agent.runner._find_backend_executable", return_value="codex"),
            patch("clud.agent.runner.get_backend", return_value=adapter),
            patch("clud.agent.runner.run_claude_process", side_effect=fake_run_claude_process),
            patch("clud.agent.runner.trigger_hook_sync"),
            patch("clud.agent.runner.run_command_hook", return_value=MagicMock(failed=False, stdout="hook\x15 output\x08\x1b[31mwith ansi\x1b[0m", stderr="")),
            patch("clud.agent.runner._wrap_command_for_git_bash", side_effect=self._identity_command),
            patch("sys.stdin.isatty", return_value=True),
        ):
            result = run_agent(args)

        self.assertEqual(result, 0)

    def test_codex_interactive_without_tty_fails_fast(self) -> None:
        args = Args(mode=AgentMode.DEFAULT, agent_backend="codex", no_skills=True, debug_tty=True)
        adapter = MagicMock()
        adapter.build_launch_plan.return_value = LaunchPlan(
            backend="codex",
            executable="codex",
            cwd=os.getcwd(),
            interactive=True,
        )

        stderr = StringIO()
        with (
            patch("clud.agent.runner.register_hooks_from_config", return_value=HookRegistrationSummary()),
            patch("clud.agent.runner._find_backend_executable", return_value="codex"),
            patch("clud.agent.runner.get_backend", return_value=adapter),
            patch("sys.stdin.isatty", return_value=False),
            patch("sys.stderr", stderr),
        ):
            result = run_agent(args)

        self.assertEqual(result, 2)
        self.assertIn("TTY DEBUG", stderr.getvalue())
        self.assertIn("stdin.isatty=False", stderr.getvalue())
        self.assertIn("requires a terminal on stdin", stderr.getvalue())

    def test_debug_tty_reports_idle_pty_codex_launch_path(self) -> None:
        args = Args(mode=AgentMode.DEFAULT, agent_backend="codex", no_skills=True, debug_tty=True)
        adapter = MagicMock()
        adapter.build_launch_plan.return_value = LaunchPlan(
            backend="codex",
            executable="codex",
            cwd=os.getcwd(),
            interactive=True,
        )

        stderr = StringIO()
        with (
            patch("clud.agent.runner.register_hooks_from_config", return_value=HookRegistrationSummary()),
            patch("clud.agent.runner.get_codex_stop_hook_idle_timeout", return_value=10.0),
            patch("clud.agent.runner._find_backend_executable", return_value="codex"),
            patch("clud.agent.runner.get_backend", return_value=adapter),
            patch("clud.agent.runner.run_claude_process", return_value=0),
            patch("clud.agent.runner.trigger_hook_sync"),
            patch("clud.agent.runner._wrap_command_for_git_bash", side_effect=self._identity_command),
            patch("sys.stdin.isatty", return_value=True),
            patch("sys.stdout.isatty", return_value=True),
            patch("sys.stderr.isatty", return_value=True),
            patch("sys.stderr", stderr),
        ):
            result = run_agent(args)

        self.assertEqual(result, 0)
        self.assertIn("TTY DEBUG", stderr.getvalue())
        self.assertIn("backend=codex", stderr.getvalue())
        self.assertIn("launch_mode=interactive-pty-idle-default", stderr.getvalue())
        self.assertIn("codex_interactive_default_pty=True", stderr.getvalue())
        self.assertIn("git_bash_wrap_applied=False", stderr.getvalue())

    def test_codex_compat_start_hooks_run_on_agent_start(self) -> None:
        args = Args(mode=AgentMode.DEFAULT, agent_backend="codex", no_skills=True)
        adapter = MagicMock()
        adapter.build_launch_plan.return_value = LaunchPlan(
            backend="codex",
            executable="codex",
            cwd=os.getcwd(),
            interactive=True,
        )
        events: list[HookEvent] = []

        def record_event(event: HookEvent, context: HookContext, hook_debug: bool = False) -> None:
            events.append(event)

        with (
            patch(
                "clud.agent.runner.register_hooks_from_config",
                return_value=HookRegistrationSummary(has_start_hooks=True),
            ),
            patch("clud.agent.runner._find_backend_executable", return_value="codex"),
            patch("clud.agent.runner.get_backend", return_value=adapter),
            patch("clud.agent.runner.run_claude_process", return_value=0),
            patch("clud.agent.runner.trigger_hook_sync", side_effect=record_event),
            patch("clud.agent.runner._wrap_command_for_git_bash", side_effect=self._identity_command),
            patch("sys.stdin.isatty", return_value=True),
        ):
            result = run_agent(args)

        self.assertEqual(result, 0)
        self.assertGreaterEqual(events.count(HookEvent.AGENT_START), 1)

    def test_no_hooks_skips_registration_and_triggering_for_codex(self) -> None:
        args = Args(mode=AgentMode.DEFAULT, agent_backend="codex", no_skills=True, no_hooks=True)
        adapter = MagicMock()
        adapter.build_launch_plan.return_value = LaunchPlan(
            backend="codex",
            executable="codex",
            cwd=os.getcwd(),
            interactive=True,
        )

        with (
            patch("clud.agent.runner.register_hooks_from_config") as mock_register,
            patch("clud.agent.runner._find_backend_executable", return_value="codex"),
            patch("clud.agent.runner.get_backend", return_value=adapter),
            patch("clud.agent.runner.run_claude_process", return_value=0),
            patch("clud.agent.runner.trigger_hook_sync") as mock_trigger,
            patch("clud.agent.runner._wrap_command_for_git_bash", side_effect=self._identity_command),
            patch("sys.stdin.isatty", return_value=True),
        ):
            result = run_agent(args)

        self.assertEqual(result, 0)
        mock_register.assert_not_called()
        mock_trigger.assert_not_called()

    def test_no_hooks_skips_registration_and_triggering_for_claude(self) -> None:
        args = Args(mode=AgentMode.DEFAULT, agent_backend="claude", no_skills=True, no_hooks=True)
        adapter = MagicMock()
        adapter.build_launch_plan.return_value = LaunchPlan(
            backend="claude",
            executable="claude",
            cwd=os.getcwd(),
            interactive=True,
        )

        with (
            patch("clud.agent.runner.register_hooks_from_config") as mock_register,
            patch("clud.agent.runner._find_backend_executable", return_value="claude"),
            patch("clud.agent.runner.get_backend", return_value=adapter),
            patch("clud.agent.runner.run_claude_process", return_value=0),
            patch("clud.agent.runner.trigger_hook_sync") as mock_trigger,
            patch("clud.agent.runner._wrap_command_for_git_bash", side_effect=self._identity_command),
        ):
            result = run_agent(args)

        self.assertEqual(result, 0)
        mock_register.assert_not_called()
        mock_trigger.assert_not_called()

    def test_start_hook_uses_devnull_stdin_and_interactive_codex_launch_still_runs(self) -> None:
        args = Args(mode=AgentMode.DEFAULT, agent_backend="codex", no_skills=True)
        order: list[str] = []
        adapter = MagicMock()
        adapter.build_launch_plan.return_value = LaunchPlan(
            backend="codex",
            executable="codex",
            cwd=os.getcwd(),
            interactive=True,
        )

        with TemporaryDirectory() as tmp:
            repo_root = Path(tmp)
            claude_dir = repo_root / ".claude"
            claude_dir.mkdir()
            (claude_dir / "settings.local.json").write_text(
                json.dumps(
                    {
                        "hooks": {
                            "Start": [
                                {
                                    "matcher": "*",
                                    "hooks": [
                                        {
                                            "type": "command",
                                            "command": "python -c \"print('start hook ran')\"",
                                        }
                                    ],
                                }
                            ]
                        }
                    }
                ),
                encoding="utf-8",
            )

            def fake_hook_run(*_args: object, **kwargs: object) -> MagicMock:
                order.append("hook")
                self.assertEqual(kwargs["shell"], True)
                cwd = cast(str | Path, kwargs["cwd"])
                self.assertIsInstance(cwd, str | Path)
                self.assertEqual(Path(cwd), repo_root)
                return MagicMock(returncode=0, stdout="", stderr="")

            def fake_launch(cmd: list[str], **_kwargs: object) -> int:
                order.append("launch")
                self.assertEqual(cmd[0], "codex")
                return 0

            previous_cwd = os.getcwd()
            try:
                os.chdir(repo_root)
                with (
                    patch("clud.agent.runner._find_backend_executable", return_value="codex"),
                    patch("clud.agent.runner.get_backend", return_value=adapter),
                    patch("clud.agent.runner.run_claude_process", side_effect=fake_launch) as mock_launch,
                    patch("clud.agent.runner._wrap_command_for_git_bash", side_effect=self._identity_command),
                    patch("clud.hooks.command.run_with_input_detached", side_effect=fake_hook_run) as mock_hook_run,
                    patch("sys.stdin.isatty", return_value=True),
                ):
                    result = run_agent(args)
            finally:
                os.chdir(previous_cwd)

        self.assertEqual(result, 0)
        self.assertEqual(order[:2], ["hook", "launch"])
        mock_hook_run.assert_called_once()
        mock_launch.assert_called_once()


if __name__ == "__main__":
    unittest.main()
