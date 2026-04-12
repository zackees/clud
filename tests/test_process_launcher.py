"""Unit tests for the PTY process launcher interrupt behavior."""

import unittest
from io import StringIO
from typing import Any
from unittest.mock import MagicMock, patch

from running_process import IdleStartTrigger, InteractiveMode, WaitCallbackResult

from clud.agent.process_launcher import PtySessionResult, run_claude_process


class TestProcessLauncher(unittest.TestCase):
    """Test Ctrl-C handling in the process launcher."""

    @patch("clud.agent.process_launcher.RunningProcess.interactive")
    def test_interactive_interrupt_can_return_130_without_reraising(
        self,
        mock_interactive: MagicMock,
    ) -> None:
        """Interactive launch should return 130 when propagation is disabled."""
        proc = MagicMock()
        proc.poll.side_effect = [None, KeyboardInterrupt(), 130, 130]
        proc.pid = 12345
        mock_interactive.return_value = proc

        with patch("clud.agent.process_launcher.os.getppid", return_value=1234):
            result = run_claude_process(["codex"], propagate_keyboard_interrupt=False)

        self.assertEqual(result, 130)
        proc.send_interrupt.assert_called_once()

    @patch("clud.agent.process_launcher.RunningProcess.interactive")
    def test_interactive_interrupt_debug_logs_catch_site(
        self,
        mock_interactive: MagicMock,
    ) -> None:
        """Debug mode should print where the process launcher caught Ctrl-C."""
        proc = MagicMock()
        proc.poll.side_effect = [None, KeyboardInterrupt(), 130, 130]
        proc.pid = 12345
        mock_interactive.return_value = proc

        stderr = StringIO()
        with (
            patch("clud.agent.process_launcher.os.getppid", return_value=1234),
            patch("sys.stderr", stderr),
        ):
            result = run_claude_process(
                ["codex"],
                propagate_keyboard_interrupt=False,
                debug_keyboard_interrupt=True,
            )

        self.assertEqual(result, 130)
        self.assertIn("DEBUG: Ctrl-C caught by process launcher", stderr.getvalue())
        self.assertIn("process_launcher.py", stderr.getvalue())

    @patch("clud.agent.process_launcher.RunningProcess.interactive")
    def test_interactive_interrupt_still_reraises_by_default(
        self,
        mock_interactive: MagicMock,
    ) -> None:
        """Default behavior should preserve KeyboardInterrupt propagation."""
        proc = MagicMock()
        proc.poll.side_effect = [None, KeyboardInterrupt(), 130, 130]
        proc.pid = 12345
        mock_interactive.return_value = proc

        with (
            patch("clud.agent.process_launcher.os.getppid", return_value=1234),
            self.assertRaises(KeyboardInterrupt),
        ):
            run_claude_process(["codex"])

    @patch("clud.agent.process_launcher.RunningProcess.interactive")
    def test_windows_child_ctrl_c_mode_uses_running_process_interactive(
        self,
        mock_interactive: MagicMock,
    ) -> None:
        """Codex interactive child mode should use running-process console isolation."""
        proc = MagicMock()
        proc.poll.side_effect = [None, 0, 0]
        mock_interactive.return_value = proc

        with (
            patch("clud.agent.process_launcher.sys.platform", "win32"),
            patch("clud.agent.process_launcher.os.getppid", return_value=1234),
        ):
            result = run_claude_process(["codex"], allow_child_ctrl_c=True, propagate_keyboard_interrupt=False)

        self.assertEqual(result, 0)
        mock_interactive.assert_called_once()
        self.assertEqual(mock_interactive.call_args.args[0], ["codex"])
        self.assertEqual(mock_interactive.call_args.kwargs["mode"], InteractiveMode.CONSOLE_ISOLATED)

    @patch("clud.agent.process_launcher.RunningProcess.interactive")
    def test_running_process_interactive_interrupt_sends_interrupt_then_returns_130(
        self,
        mock_interactive: MagicMock,
    ) -> None:
        """KeyboardInterrupt in Codex interactive mode should trigger library interrupt cleanup."""

        class FakeProc:
            def __init__(self) -> None:
                self.poll_calls = 0
                self.send_interrupt_calls = 0

            def poll(self) -> int | None:
                self.poll_calls += 1
                if self.poll_calls == 1:
                    return None
                if self.poll_calls == 2:
                    raise KeyboardInterrupt()
                return 130

            def send_interrupt(self) -> None:
                self.send_interrupt_calls += 1

            def terminate(self) -> None:
                return None

            def kill(self) -> None:
                return None

        proc = FakeProc()
        mock_interactive.return_value = proc

        with (
            patch("clud.agent.process_launcher.sys.platform", "win32"),
            patch("clud.agent.process_launcher.os.getppid", return_value=1234),
        ):
            result = run_claude_process(["codex"], allow_child_ctrl_c=True, propagate_keyboard_interrupt=False)

        self.assertEqual(result, 130)
        self.assertGreaterEqual(proc.poll_calls, 2)

    @patch("clud.agent.process_launcher.RunningProcess.interactive")
    @patch("clud.agent.process_launcher.PseudoTerminalProcess")
    def test_idle_timeout_uses_pseudo_terminal_session(
        self,
        mock_pseudo_terminal: MagicMock,
        mock_interactive: MagicMock,
    ) -> None:
        """Idle detection should mark a session boundary without killing the PTY."""

        class FakeProc:
            def __init__(self) -> None:
                self.exit_reason = "exit"
                self.wait_for_kwargs: dict[str, object] = {}
                self.idle_timeout_enabled = True

            def wait_for(self, *conditions: Any, **kwargs: Any) -> MagicMock:  # noqa: ANN401
                self.wait_for_kwargs = kwargs
                idle_condition: Any = conditions[0]  # noqa: ANN401
                detector: Any = idle_condition.detector  # noqa: ANN401
                pty_config: Any = detector.pty  # noqa: ANN401
                assert getattr(pty_config, "start_trigger", None) is IdleStartTrigger.IMMEDIATE
                callback: Any = idle_condition.on_callback  # noqa: ANN401

                class InputBuffer:
                    def write(self, _data: str) -> None:
                        return None

                action: Any = callback(object(), InputBuffer())  # noqa: ANN401
                assert action is WaitCallbackResult.CONTINUE_AND_DISARM
                return MagicMock(returncode=0, callback_result=None)

            def poll(self) -> int | None:
                return 0

            def write(self, _data: str) -> None:
                return None

        proc = FakeProc()
        mock_pseudo_terminal.return_value = proc

        with (
            patch("clud.agent.process_launcher.sys.platform", "win32"),
            patch("clud.agent.process_launcher.os.getppid", return_value=1234),
        ):
            result = run_claude_process(["codex"], idle_timeout=0.1, propagate_keyboard_interrupt=False)

        self.assertIsInstance(result, PtySessionResult)
        assert isinstance(result, PtySessionResult)
        self.assertTrue(result.idle_detected)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.idle_event_count, 1)
        call_kwargs = mock_pseudo_terminal.call_args
        self.assertEqual(call_kwargs[0][0], ["codex"])
        self.assertFalse(call_kwargs[1]["capture"])
        self.assertTrue(call_kwargs[1]["relay_terminal_input"])
        self.assertTrue(call_kwargs[1]["arm_idle_timeout_on_submit"])
        # Terminal size should be forwarded from the host
        self.assertIn("rows", call_kwargs[1])
        self.assertIn("cols", call_kwargs[1])
        self.assertGreater(call_kwargs[1]["rows"], 0)
        self.assertGreater(call_kwargs[1]["cols"], 0)
        self.assertEqual(proc.idle_timeout_enabled, False)
        self.assertEqual(proc.wait_for_kwargs["echo_output"], True)
        mock_interactive.assert_not_called()

    @patch("clud.agent.process_launcher.PseudoTerminalProcess")
    def test_idle_timeout_interrupt_uses_interrupt_and_wait(
        self,
        mock_pseudo_terminal: MagicMock,
    ) -> None:
        """Ctrl-C during PTY idle mode should use interrupt escalation helpers."""

        class FakeProc:
            def __init__(self) -> None:
                self.exit_reason = "interrupt"
                self.poll_calls = 0
                self.interrupt_and_wait_called = False
                self.idle_timeout_enabled = True

            def wait_for(self, *conditions: object, **kwargs: object) -> MagicMock:
                raise KeyboardInterrupt()

            def poll(self) -> int | None:
                self.poll_calls += 1
                if self.poll_calls == 1:
                    return None
                return 130

            def interrupt_and_wait(self, **_kwargs: object) -> None:
                self.interrupt_and_wait_called = True
                return None

            def write(self, _data: str) -> None:
                return None

        proc = FakeProc()
        mock_pseudo_terminal.return_value = proc

        with (
            patch("clud.agent.process_launcher.sys.platform", "win32"),
            patch("clud.agent.process_launcher.os.getppid", return_value=1234),
        ):
            result = run_claude_process(["codex"], idle_timeout=0.1, propagate_keyboard_interrupt=False)

        self.assertIsInstance(result, PtySessionResult)
        assert isinstance(result, PtySessionResult)
        self.assertEqual(result.returncode, 130)
        self.assertTrue(proc.interrupt_and_wait_called)

    @patch("clud.agent.process_launcher.PseudoTerminalProcess")
    def test_idle_timeout_can_inject_follow_up_without_exiting(
        self,
        mock_pseudo_terminal: MagicMock,
    ) -> None:
        """Idle callback follow-up should be written into the PTY instead of exiting."""

        class FakeProc:
            def __init__(self) -> None:
                self.exit_reason = "exit"
                self.writes: list[str] = []
                self.idle_timeout_enabled = True

            def wait_for(self, *conditions: Any, **kwargs: Any) -> MagicMock:  # noqa: ANN401
                idle_condition: Any = conditions[0]  # noqa: ANN401
                callback: Any = idle_condition.on_callback  # noqa: ANN401

                class InputBuffer:
                    def __init__(self, outer: "FakeProc") -> None:
                        self.outer = outer

                    def write(self, data: str) -> None:
                        self.outer.writes.append(data)

                action: Any = callback(object(), InputBuffer(self))  # noqa: ANN401
                assert action is WaitCallbackResult.CONTINUE
                return MagicMock(returncode=0, callback_result=None)

            def poll(self) -> int | None:
                return 0

        proc = FakeProc()
        mock_pseudo_terminal.return_value = proc

        with (
            patch("clud.agent.process_launcher.sys.platform", "win32"),
            patch("clud.agent.process_launcher.os.getppid", return_value=1234),
        ):
            result = run_claude_process(
                ["codex"],
                idle_timeout=0.1,
                propagate_keyboard_interrupt=False,
                on_idle=lambda: "hook follow-up",
            )

        self.assertIsInstance(result, PtySessionResult)
        assert isinstance(result, PtySessionResult)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.idle_event_count, 1)
        self.assertEqual(proc.writes, ["hook follow-up\r"])


if __name__ == "__main__":
    unittest.main()
