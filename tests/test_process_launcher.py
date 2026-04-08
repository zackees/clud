"""Unit tests for the PTY process launcher interrupt behavior."""

import unittest
from unittest.mock import MagicMock, patch

from clud.agent.process_launcher import run_claude_process


class TestProcessLauncher(unittest.TestCase):
    """Test Ctrl-C handling in the process launcher."""

    @patch("clud.agent.process_launcher.kill_process_tree")
    @patch("clud.agent.process_launcher.subprocess.Popen")
    def test_interactive_interrupt_can_return_130_without_reraising(
        self,
        mock_popen: MagicMock,
        mock_kill_process_tree: MagicMock,
    ) -> None:
        """Interactive launch should return 130 when propagation is disabled."""
        proc = MagicMock()
        proc.poll.return_value = None
        proc.wait.side_effect = [KeyboardInterrupt(), 130]
        proc.returncode = 130
        mock_popen.return_value = proc

        with patch("clud.agent.process_launcher.os.getppid", return_value=1234):
            result = run_claude_process(["codex"], propagate_keyboard_interrupt=False)

        self.assertEqual(result, 130)
        mock_kill_process_tree.assert_called_once_with(proc.pid)

    @patch("clud.agent.process_launcher.kill_process_tree")
    @patch("clud.agent.process_launcher.subprocess.Popen")
    def test_interactive_interrupt_still_reraises_by_default(
        self,
        mock_popen: MagicMock,
        mock_kill_process_tree: MagicMock,
    ) -> None:
        """Default behavior should preserve KeyboardInterrupt propagation."""
        proc = MagicMock()
        proc.poll.return_value = None
        proc.wait.side_effect = [KeyboardInterrupt(), 130]
        proc.returncode = 130
        mock_popen.return_value = proc

        with (
            patch("clud.agent.process_launcher.os.getppid", return_value=1234),
            self.assertRaises(KeyboardInterrupt),
        ):
            run_claude_process(["codex"])

        mock_kill_process_tree.assert_called_once_with(proc.pid)


if __name__ == "__main__":
    unittest.main()
