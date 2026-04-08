"""Tests for CLI integration with yolo functionality."""

import unittest
from io import StringIO
from unittest.mock import patch

from clud.cli import main


class TestCliYoloIntegration(unittest.TestCase):
    """Test CLI integration with yolo functionality."""

    def test_cli_with_message_and_dry_run(self) -> None:
        """Test CLI with -m and --dry-run flags."""
        # Capture stdout
        captured_output = StringIO()
        with patch("sys.stdout", captured_output):
            result = main(["-m", "test message from CLI", "--dry-run"])

        self.assertEqual(result, 0)
        self.assertTrue(captured_output.getvalue().strip().startswith("Would execute: claude --dangerously-skip-permissions test message from CLI"))

    def test_cli_dry_run_without_message(self) -> None:
        """Test CLI with --dry-run but no message."""
        # Capture stdout
        captured_output = StringIO()
        with patch("sys.stdout", captured_output):
            result = main(["--dry-run"])

        self.assertEqual(result, 0)
        self.assertTrue(captured_output.getvalue().strip().startswith("Would execute: claude --dangerously-skip-permissions"))

    def test_cli_message_without_dry_run_mocked(self) -> None:
        """Test CLI with -m but no --dry-run (should try to run Claude, but we mock it)."""
        # Mock run_claude_process to avoid actually running Claude
        # Interactive mode (no -p flag) uses run_claude_process for process group isolation
        with (
            patch("clud.agent.runner.run_claude_process", return_value=0) as mock_run,
            patch("clud.agent.runner._find_claude_path", return_value="/fake/claude"),
            patch("clud.agent.runner._wrap_command_for_git_bash", side_effect=lambda cmd: cmd),  # type: ignore[misc]
        ):
            result = main(["-m", "test message"])

        self.assertEqual(result, 0)
        # Verify that run_claude_process was called (meaning it tried to run Claude)
        self.assertEqual(mock_run.call_count, 1)
        # Verify the call included claude and the message
        call_args = mock_run.call_args
        cmd = call_args[0][0]  # First positional arg is the command list
        self.assertIn("claude", str(cmd))
        self.assertIn("test message", str(cmd))

    def test_cli_interactive_uses_non_propagating_process_launcher(self) -> None:
        """Interactive mode should suppress KeyboardInterrupt propagation from the PTY launcher."""
        with (
            patch("clud.agent.runner.run_claude_process", return_value=130) as mock_run,
            patch("clud.agent.runner._find_claude_path", return_value="/fake/claude"),
            patch("clud.agent.runner._wrap_command_for_git_bash", side_effect=lambda cmd: cmd),  # type: ignore[misc]
            patch("sys.stdin.isatty", return_value=True),
        ):
            result = main([])

        self.assertEqual(result, 130)
        self.assertEqual(mock_run.call_count, 1)
        self.assertFalse(mock_run.call_args.kwargs["propagate_keyboard_interrupt"])


if __name__ == "__main__":
    unittest.main()
